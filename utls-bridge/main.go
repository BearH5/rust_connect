package main

/*
#include <stdlib.h>
#include <string.h>
*/
import "C"

import (
	"sync"
	"unsafe"
)

// 最近一次错误，供 Rust 侧通过 ec_last_error 读取。
// cgo 不能把 Go 字符串直接传出，故存到 Go 侧并用 C.CString 暴露指针。
var (
	lastErrMu  sync.Mutex
	lastErrPtr = (*C.char)(nil)
)

func setLastError(err error) {
	lastErrMu.Lock()
	defer lastErrMu.Unlock()
	if lastErrPtr != nil {
		C.free(unsafe.Pointer(lastErrPtr))
	}
	if err == nil {
		lastErrPtr = (*C.char)(nil)
		return
	}
	lastErrPtr = C.CString(err.Error())
}

//export ec_handshake
// 建立 TLS 连接，返回句柄（>0 成功，<=0 失败）。
// mode: 0=普通(HelloGolang), 1=特殊(L3IP/RC4/伪扩展)
func ec_handshake(server *C.char, mode C.int) C.longlong {
	conn, _, err := dialAndHandshake(C.GoString(server), int(mode))
	if err != nil {
		setLastError(err)
		return -1
	}
	setLastError(nil)
	return C.longlong(registerConn(conn))
}

//export ec_last_error
// 返回最近一次 ec_handshake 错误的 C 字符串（NULL 表示无错误）。
// 调用方不应 free；指针由库内部管理。
func ec_last_error() *C.char {
	lastErrMu.Lock()
	defer lastErrMu.Unlock()
	return lastErrPtr
}

//export ec_conn_session_id
// 读取该连接 ServerHello 的 session_id，写入 buf，返回长度（<0 失败）。
func ec_conn_session_id(handle C.longlong, buf *C.char, bufLen C.longlong) C.longlong {
	conn := getConn(int64(handle))
	if conn == nil {
		return -1
	}
	sid := conn.HandshakeState.ServerHello.SessionId
	if int64(len(sid)) > int64(bufLen) {
		return -1
	}
	C.memcpy(unsafe.Pointer(buf), unsafe.Pointer(&sid[0]), C.size_t(len(sid)))
	return C.longlong(len(sid))
}

//export ec_conn_read
// 从连接读字节，返回读取数（<0 失败，0 EOF）。
func ec_conn_read(handle C.longlong, buf *C.char, bufLen C.longlong) C.longlong {
	conn := getConn(int64(handle))
	if conn == nil {
		return -1
	}
	n, err := conn.Read((*[1 << 30]byte)(unsafe.Pointer(buf))[:bufLen:bufLen])
	if err != nil {
		if n == 0 {
			return -1
		}
	}
	return C.longlong(n)
}

//export ec_conn_write
// 向连接写字节，返回写入数（<0 失败）。
func ec_conn_write(handle C.longlong, buf *C.char, bufLen C.longlong) C.longlong {
	conn := getConn(int64(handle))
	if conn == nil {
		return -1
	}
	n, err := conn.Write((*[1 << 30]byte)(unsafe.Pointer(buf))[:bufLen:bufLen])
	if err != nil {
		if n == 0 {
			return -1
		}
	}
	return C.longlong(n)
}

//export ec_conn_close
// 关闭并移除连接。
func ec_conn_close(handle C.longlong) {
	if conn := takeConn(int64(handle)); conn != nil {
		conn.Close()
	}
}

func main() {}
