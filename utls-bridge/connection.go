package main

import (
	"crypto/rand"
	"io"
	"net"
	"sync"

	utls "github.com/refraction-networking/utls"
)

// 全局连接表。cgo 不能把 Go 指针传给 Rust，故用整数句柄。
var (
	connMu    sync.Mutex
	connTable = make(map[int64]*utls.UConn)
	connNext  int64 = 1
)

// 存入连接，返回句柄。
func registerConn(conn *utls.UConn) int64 {
	connMu.Lock()
	defer connMu.Unlock()
	handle := connNext
	connNext++
	connTable[handle] = conn
	return handle
}

// 取出连接。返回 nil 表示句柄无效。
func getConn(handle int64) *utls.UConn {
	connMu.Lock()
	defer connMu.Unlock()
	return connTable[handle]
}

// 删除并返回连接。
func takeConn(handle int64) *utls.UConn {
	connMu.Lock()
	defer connMu.Unlock()
	conn := connTable[handle]
	delete(connTable, handle)
	return conn
}

const (
	modeNormal  = 0 // utls.HelloGolang，用于 requestToken
	modeSpecial = 1 // utls.HelloCustom，用于 tlsConn（L3IP/RC4/伪扩展）
)

// dialAndHandshake 建立 TLS 连接并完成握手。
// mode=0 普通(HelloGolang)，mode=1 特殊(L3IP+RC4+伪扩展)。
// 返回连接和 ServerHello 的 session_id。
func dialAndHandshake(server string, mode int) (*utls.UConn, []byte, error) {
	dialConn, err := net.Dial("tcp", server)
	if err != nil {
		return nil, nil, err
	}

	var conn *utls.UConn
	if mode == modeNormal {
		// 普通 TLS：对照 request.go:572 HelloGolang
		conn = utls.UClient(dialConn, &utls.Config{InsecureSkipVerify: true}, utls.HelloGolang)
		if err := conn.Handshake(); err != nil {
			dialConn.Close()
			return nil, nil, err
		}
	} else {
		// 特殊 TLS：逐字段复刻 protocol.go:63-73
		conn = utls.UClient(dialConn, &utls.Config{InsecureSkipVerify: true}, utls.HelloCustom)

		// protocol.go:65-67 固定 ClientRandom
		random := make([]byte, 32)
		_, _ = rand.Read(random)
		_ = conn.SetClientRandom(random)
		// protocol.go:68 固定 TLS1.1
		_ = conn.SetTLSVers(utls.VersionTLS11, utls.VersionTLS11, []utls.TLSExtension{})
		// protocol.go:69 ClientHello 版本字段
		conn.HandshakeState.Hello.Vers = utls.VersionTLS11
		// protocol.go:70 仅 RC4-SHA + 伪 SCSV
		conn.HandshakeState.Hello.CipherSuites = []uint16{
			utls.TLS_RSA_WITH_RC4_128_SHA,
			utls.FAKE_TLS_EMPTY_RENEGOTIATION_INFO_SCSV,
		}
		// protocol.go:71 压缩方法 null
		conn.HandshakeState.Hello.CompressionMethods = []uint8{0}
		// protocol.go:72 SessionId = "L3IP" + 28个0（必须恰好 32 字节，Sangfor 据此区分 VPN 隧道）
		conn.HandshakeState.Hello.SessionId = []byte{'L', '3', 'I', 'P', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0}
		// protocol.go:73 伪造 Heartbeat 扩展
		conn.Extensions = []utls.TLSExtension{&fakeHeartBeatExtension{}}

		if err := conn.Handshake(); err != nil {
			dialConn.Close()
			return nil, nil, err
		}
	}

	// request.go:591 读取 ServerHello 的 SessionId
	sessionID := conn.HandshakeState.ServerHello.SessionId
	return conn, sessionID, nil
}

// fakeHeartBeatExtension 复刻 protocol.go:33-50。
// 构造一个内容硬编码的伪 Heartbeat 扩展。
type fakeHeartBeatExtension struct {
	*utls.GenericExtension
}

func (e *fakeHeartBeatExtension) Len() int {
	return 5
}

func (e *fakeHeartBeatExtension) Read(b []byte) (n int, err error) {
	if len(b) < e.Len() {
		return 0, io.ErrShortBuffer
	}
	// protocol.go:45-47 固定字节
	b[1] = 0x0f
	b[3] = 1
	b[4] = 1
	return e.Len(), io.EOF
}
