.DEFAULT_GOAL := app

.PHONY: app mac-arm64 mac-amd64 windows linux require-macos require-windows require-linux unsupported

ifeq ($(OS),Windows_NT)
HOST_OS := Windows_NT
HOST_ARCH := $(PROCESSOR_ARCHITECTURE)
ifeq ($(HOST_ARCH),AMD64)
APP_TARGET := windows
endif
else
HOST_OS := $(shell uname -s)
HOST_ARCH := $(shell uname -m)
ifeq ($(HOST_OS),Darwin)
ifeq ($(HOST_ARCH),arm64)
APP_TARGET := mac-arm64
else ifeq ($(HOST_ARCH),x86_64)
APP_TARGET := mac-amd64
endif
else ifeq ($(HOST_OS),Linux)
ifeq ($(HOST_ARCH),x86_64)
APP_TARGET := linux
endif
endif
endif

app:
	@$(MAKE) $(if $(APP_TARGET),$(APP_TARGET),unsupported)

mac-arm64: require-macos
	npm run tauri -- build --target aarch64-apple-darwin --bundles app

mac-amd64: require-macos
	npm run tauri -- build --target x86_64-apple-darwin --bundles app

windows: require-windows
	npm run tauri -- build --target x86_64-pc-windows-msvc --bundles nsis

linux: require-linux
	npm run tauri -- build --target x86_64-unknown-linux-gnu --bundles deb

require-macos:
ifneq ($(HOST_OS),Darwin)
	@$(error macOS build requires a macOS host)
endif

require-windows:
ifneq ($(HOST_OS),Windows_NT)
	@$(error Windows build requires a Windows host)
else ifneq ($(HOST_ARCH),AMD64)
	@$(error Windows build requires an x64 host)
endif

require-linux:
ifneq ($(HOST_OS),Linux)
	@$(error Linux build requires a Linux host)
else ifneq ($(HOST_ARCH),x86_64)
	@$(error Linux build requires an x64 host)
endif

unsupported:
	@$(error Unsupported host: $(HOST_OS) $(HOST_ARCH))
