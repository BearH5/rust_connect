@echo off
REM 构建 utls-bridge 为 c-shared dll。
REM 需 cgo，用 mingw64 的 gcc 作为 CC。
set CGO_ENABLED=1
set CC=D:\dev_evn\mingw64\bin\gcc.exe
set GOPATH=D:\dev_evn\gopath
set GOTOOLCHAIN=local
set PATH=D:\dev_evn\Go\bin;D:\dev_evn\mingw64\bin;%PATH%
go build -buildmode=c-shared -o ec_utls_bridge.dll .
if errorlevel 1 (
    echo 构建失败
    exit /b 1
)
REM mingw c-shared 不生成导入库，用 gendef+dlltool 从 dll 生成供 Rust 链接
gendef ec_utls_bridge.dll
dlltool -d ec_utls_bridge.def -l libec_utls_bridge.a -k
echo 构建完成：ec_utls_bridge.dll + ec_utls_bridge.h + libec_utls_bridge.a
