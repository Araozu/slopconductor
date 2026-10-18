# Release build + install for `hived` (hive-daemon)
#
# Usage:
#   make            # release build
#   make build      # release build
#   make install    # build + install to ~/.local/bin/hived
#   make uninstall  # remove installed binary
#   make clean      # cargo clean
#
# Overrides:
#   make install PREFIX=/usr/local        # -> /usr/local/bin/hived
#   make install BINDIR=~/bin             # custom dir
#   make build CARGO_BUILD_FLAGS="--locked"

CARGO ?= cargo
PACKAGE := hive-daemon
BIN_NAME := hived

PROFILE ?= release
TARGET_DIR ?= target
SRC_BIN := $(TARGET_DIR)/$(PROFILE)/$(BIN_NAME)

PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
DEST := $(BINDIR)/$(BIN_NAME)

CARGO_BUILD_FLAGS ?=

# User systemd unit
SYSTEMD_SRC := systemd/hived.service
SYSTEMD_USER_DIR ?= $(HOME)/.config/systemd/user
UNIT := $(SYSTEMD_USER_DIR)/hived.service
UNIT_NAME := hived.service
SYSTEMCTL ?= systemctl
# Must match DEST for the default PREFIX. Override both together for custom paths,
# e.g. make install BINDIR=/opt/hive/bin install-unit SYSTEMD_EXEC=/opt/hive/bin/hived
SYSTEMD_EXEC ?= %h/.local/bin/hived

.PHONY: all build install uninstall clean help install-unit uninstall-unit enable disable start stop restart status logs user-install

all: build

build:
	$(CARGO) build --profile $(PROFILE) -p $(PACKAGE) $(CARGO_BUILD_FLAGS)

install: build
	mkdir -p "$(BINDIR)"
	install -m 0755 "$(SRC_BIN)" "$(DEST)"
	@echo "installed $(DEST)"

uninstall:
	rm -f "$(DEST)"
	@echo "removed $(DEST)"

clean:
	$(CARGO) clean

# Binary must exist before the unit can run, so install-unit depends on install.
install-unit: install
	mkdir -p "$(SYSTEMD_USER_DIR)"
	sed -e "s|^ExecStart=.*|ExecStart=$(SYSTEMD_EXEC)|" "$(SYSTEMD_SRC)" > "$(UNIT)"
	chmod 0644 "$(UNIT)"
	$(SYSTEMCTL) --user daemon-reload
	@echo "installed $(UNIT)"
	@echo "enable with: make enable"

uninstall-unit:
	-$(SYSTEMCTL) --user disable --now "$(UNIT_NAME)"
	rm -f "$(UNIT)"
	$(SYSTEMCTL) --user daemon-reload
	@echo "removed $(UNIT)"

enable:
	$(SYSTEMCTL) --user enable --now "$(UNIT_NAME)"

disable:
	$(SYSTEMCTL) --user disable --now "$(UNIT_NAME)"

start:
	$(SYSTEMCTL) --user start "$(UNIT_NAME)"

stop:
	$(SYSTEMCTL) --user stop "$(UNIT_NAME)"

restart:
	$(SYSTEMCTL) --user restart "$(UNIT_NAME)"

status:
	$(SYSTEMCTL) --user status "$(UNIT_NAME)" --no-pager

logs:
	journalctl --user -u "$(UNIT_NAME)" -f

# Full user setup: binary + unit + enable --now
user-install: install-unit enable

help:
	@echo "Targets:"
	@echo "  all / build   - cargo build --profile $(PROFILE) -p $(PACKAGE)"
	@echo "  install       - build + install to $(DEST)"
	@echo "  uninstall     - rm -f $(DEST)"
	@echo "  install-unit  - install binary + user unit to $(UNIT)"
	@echo "  uninstall-unit- disable + remove user unit"
	@echo "  enable        - systemctl --user enable --now $(UNIT_NAME)"
	@echo "  disable/start/stop/restart/status - unit controls"
	@echo "  logs          - journalctl --user -u $(UNIT_NAME) -f"
	@echo "  user-install  - install + install-unit + enable"
	@echo "  clean         - cargo clean"
	@echo ""
	@echo "Variables: PROFILE=$(PROFILE) PREFIX=$(PREFIX) BINDIR=$(BINDIR)"
