# Makefile for cross-compiling Sharetty

APP_NAME = sharetty
OUT_DIR = bin

# Linux Targets (Static with musl where possible)
TARGET_LINUX_X64 = x86_64-unknown-linux-musl
TARGET_LINUX_X86 = i686-unknown-linux-musl
TARGET_LINUX_ARM64 = aarch64-unknown-linux-musl
TARGET_LINUX_ARMV7 = armv7-unknown-linux-musleabihf
TARGET_LINUX_RISCV64 = riscv64gc-unknown-linux-gnu

# Windows Targets (GNU)
TARGET_WIN_X64 = x86_64-pc-windows-gnu
# TARGET_WIN_X86 = i686-pc-windows-gnu

# Windows Targets (MSVC)
TARGET_WIN_MSVC_X64 = x86_64-pc-windows-msvc
TARGET_WIN_MSVC_X86 = i686-pc-windows-msvc
TARGET_WIN_MSVC_ARM64 = aarch64-pc-windows-msvc

# macOS Targets
TARGET_MAC_X64 = x86_64-apple-darwin
TARGET_MAC_ARM64 = aarch64-apple-darwin

# BSD Targets
TARGET_FREEBSD_X64 = x86_64-unknown-freebsd
# NetBSD fails due to pty-process dependency issue
# TARGET_NETBSD_X64 = x86_64-unknown-netbsd
# OpenBSD fails due to lack of cross docker image
# TARGET_OPENBSD_X64 = x86_64-unknown-openbsd

# Group Targets
LINUX_TARGETS = $(TARGET_LINUX_X64) $(TARGET_LINUX_X86) $(TARGET_LINUX_ARM64) $(TARGET_LINUX_ARMV7) $(TARGET_LINUX_RISCV64)
WINDOWS_TARGETS = $(TARGET_WIN_X64)
MSVC_TARGETS = $(TARGET_WIN_MSVC_X64) $(TARGET_WIN_MSVC_X86) $(TARGET_WIN_MSVC_ARM64)
MAC_TARGETS = 
MACOS_TARGETS = $(TARGET_MAC_X64) $(TARGET_MAC_ARM64)
BSD_TARGETS = $(TARGET_FREEBSD_X64)

ALL_TARGETS = $(LINUX_TARGETS) $(WINDOWS_TARGETS) $(MAC_TARGETS) $(BSD_TARGETS)

.PHONY: all build release dist clean list msvc macos

# Strip debug symbols by default (use STRIP=0 to disable)
STRIP ?= 1

RUSTFLAGS_COMMON :=
ifeq ($(STRIP), 1)
	RUSTFLAGS_COMMON += -C strip=symbols
endif

# Default target (dev build)
all: build

build:
	cargo build

# Release build (native)
release:
	cargo build --release

# Cross-compile all targets
dist: $(ALL_TARGETS)

msvc: $(MSVC_TARGETS)

macos: $(MACOS_TARGETS)

define BUILD_TARGET
$(1):
	@echo "Building for $(1)..."
	CARGO_TARGET_DIR=target/$(1) RUSTFLAGS="$(RUSTFLAGS_COMMON)" cross build --release --target $(1)
	@mkdir -p $(OUT_DIR)/$(1)
	@cp target/$(1)/$(1)/release/$(APP_NAME)* $(OUT_DIR)/$(1)/
endef

# Build rule for Windows targets (static CRT)
define BUILD_WINDOWS_TARGET
$(1):
	@echo "Building for $(1) (Windows)..."
	CARGO_TARGET_DIR=target/$(1) RUSTFLAGS="-C target-feature=+crt-static $(RUSTFLAGS_COMMON)" cross build --release --target $(1)
	@mkdir -p $(OUT_DIR)/$(1)
	@cp target/$(1)/$(1)/release/$(APP_NAME).exe $(OUT_DIR)/$(1)/
endef

# MSVC Targets (for GitHub Actions / Windows env)
define BUILD_MSVC_TARGET
$(1):
	@echo "Building for $(1) (Windows MSVC)..."
	CARGO_TARGET_DIR=target/$(1) RUSTFLAGS="-C target-feature=+crt-static $(RUSTFLAGS_COMMON)" cargo build --release --target $(1)
	@mkdir -p $(OUT_DIR)/$(1)
	@cp target/$(1)/$(1)/release/$(APP_NAME).exe $(OUT_DIR)/$(1)/
endef

# macOS Targets (for GitHub Actions / macOS env)
define BUILD_MACOS_TARGET
$(1):
	@echo "Building for $(1) (macOS)..."
	CARGO_TARGET_DIR=target/$(1) RUSTFLAGS="$(RUSTFLAGS_COMMON)" cargo build --release --target $(1)
	@mkdir -p $(OUT_DIR)/$(1)
	@cp target/$(1)/$(1)/release/$(APP_NAME) $(OUT_DIR)/$(1)/
endef

$(foreach target,$(MSVC_TARGETS),$(eval $(call BUILD_MSVC_TARGET,$(target))))
$(foreach target,$(MACOS_TARGETS),$(eval $(call BUILD_MACOS_TARGET,$(target))))

# Generate build rules
$(foreach target,$(LINUX_TARGETS),$(eval $(call BUILD_TARGET,$(target))))
$(foreach target,$(MAC_TARGETS),$(eval $(call BUILD_TARGET,$(target))))
$(foreach target,$(BSD_TARGETS),$(eval $(call BUILD_TARGET,$(target))))
$(foreach target,$(WINDOWS_TARGETS),$(eval $(call BUILD_WINDOWS_TARGET,$(target))))

clean:
	cargo clean
	rm -rf $(OUT_DIR)

list:
	@echo "Available targets:"
	@echo "Linux:   $(LINUX_TARGETS)"
	@echo "Windows: $(WINDOWS_TARGETS)"
	@echo "macOS:   $(MAC_TARGETS)"
	@echo "BSD:     $(BSD_TARGETS)"
