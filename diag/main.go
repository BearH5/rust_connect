package main

// 诊断程序：把 utls-bridge 的 connection.go 逻辑复制过来，
// 编译成普通 exe（不是 dll），直接连服务器测特殊模式握手。
// 目的：分离「代码逻辑问题」vs「cgo/dll 环境问题」。

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"io"
	"net"
	"os"

	utls "github.com/refraction-networking/utls"
)

func main() {
	server := "1.2.3.4:44333"
	if len(os.Args) > 1 {
		server = os.Args[1]
	}

	fmt.Println("=== 测试普通模式 (HelloGolang) ===")
	testMode(server, 0)

	fmt.Println("\n=== 测试特殊模式 (HelloCustom/RC4/L3IP) ===")
	testMode(server, 1)
}

func testMode(server string, mode int) {
	dialConn, err := net.Dial("tcp", server)
	if err != nil {
		fmt.Printf("TCP 连接失败: %v\n", err)
		return
	}
	defer dialConn.Close()
	fmt.Printf("TCP 已连接到 %s\n", server)

	var conn *utls.UConn
	if mode == 0 {
		conn = utls.UClient(dialConn, &utls.Config{InsecureSkipVerify: true}, utls.HelloGolang)
	} else {
		conn = utls.UClient(dialConn, &utls.Config{InsecureSkipVerify: true}, utls.HelloCustom)
		random := make([]byte, 32)
		_, _ = rand.Read(random)
		_ = conn.SetClientRandom(random)
		_ = conn.SetTLSVers(utls.VersionTLS11, utls.VersionTLS11, []utls.TLSExtension{})
		conn.HandshakeState.Hello.Vers = utls.VersionTLS11
		conn.HandshakeState.Hello.CipherSuites = []uint16{
			utls.TLS_RSA_WITH_RC4_128_SHA,
			utls.FAKE_TLS_EMPTY_RENEGOTIATION_INFO_SCSV,
		}
		conn.HandshakeState.Hello.CompressionMethods = []uint8{0}
		conn.HandshakeState.Hello.SessionId = []byte{'L', '3', 'I', 'P', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0}
		conn.Extensions = []utls.TLSExtension{&fakeHeartBeatExtension{}}
	}

	// dump ClientHello 字节（关键诊断信息）
	if conn.HandshakeState.Hello != nil {
		fmt.Printf("ClientHello Vers: 0x%04x\n", conn.HandshakeState.Hello.Vers)
		fmt.Printf("CipherSuites: %x\n", conn.HandshakeState.Hello.CipherSuites)
		fmt.Printf("SessionId (%d bytes): %s\n", len(conn.HandshakeState.Hello.SessionId), hex.EncodeToString(conn.HandshakeState.Hello.SessionId))
		fmt.Printf("Extensions count: %d\n", len(conn.Extensions))
	}

	err = conn.Handshake()
	if err != nil {
		fmt.Printf("握手失败: %v\n", err)
		return
	}
	fmt.Printf("握手成功！session_id: %s\n", hex.EncodeToString(conn.HandshakeState.ServerHello.SessionId))
}

type fakeHeartBeatExtension struct {
	*utls.GenericExtension
}

func (e *fakeHeartBeatExtension) Len() int { return 5 }
func (e *fakeHeartBeatExtension) Read(b []byte) (n int, err error) {
	if len(b) < e.Len() {
		return 0, io.ErrShortBuffer
	}
	b[1] = 0x0f
	b[3] = 1
	b[4] = 1
	return e.Len(), io.EOF
}
