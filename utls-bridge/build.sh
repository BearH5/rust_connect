#!/bin/bash
# 构建 utls-bridge 为 c-shared so（Linux）。
# 需 cgo，用系统 gcc。Go 源码与 Windows 版完全相同。
set -e
export CGO_ENABLED=1
go build -buildmode=c-shared -ldflags "-s -w" -o ec_utls_bridge.so .
# c-shared 模式自动生成 ec_utls_bridge.h + ec_utls_bridge.so
# 不需要 gendef/dlltool（那是 mingw 特有）
echo "构建完成：ec_utls_bridge.so + ec_utls_bridge.h"
