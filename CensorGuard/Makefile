PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
SBINDIR ?= $(PREFIX)/sbin
AGENTLIBDIR ?= $(PREFIX)/lib/censorguard
SYSCONFDIR ?= /etc/censorguard
SYSTEMDUNITDIR ?= $(PREFIX)/lib/systemd/system
SYSUSERSDIR ?= $(PREFIX)/lib/sysusers.d
TMPFILESDIR ?= $(PREFIX)/lib/tmpfiles.d
DOCDIR ?= $(PREFIX)/share/doc/censorguard
LICENSEDIR ?= $(PREFIX)/share/licenses/censorguard
DESTDIR ?=
DISTDIR ?= target/dist
VERSION ?= $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
ARCH ?= $(shell uname -m)
BPF_ARCH ?= $(shell uname -m | sed -e 's/x86_64/x86/' -e 's/aarch64/arm64/')
VMLINUX_BTF ?= /sys/kernel/btf/vmlinux
INSTALL ?= install

.NOTPARALLEL: install package integration-install integration

.PHONY: all check test bpf vmlinux build install install-files uninstall package startup-test clean

all: check bpf build

check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --offline -- -D warnings
	bash -n scripts/*.sh
	@for source in tools/*.c; do \
		$(CC) -O2 -Wall -Wextra -Werror -fsyntax-only "$$source"; \
	done

test:
	cargo test --workspace --offline

bpf: vmlinux
	clang -target bpf -D__TARGET_ARCH_$(BPF_ARCH) -O2 -g -Wall -Werror \
		-I./bpf -I/usr/include -c bpf/enforce.bpf.c -o bpf/enforce.bpf.o.tmp
	llvm-strip -g bpf/enforce.bpf.o.tmp
	mv -f bpf/enforce.bpf.o.tmp bpf/enforce.bpf.o

vmlinux:
	bpftool btf dump file $(VMLINUX_BTF) format c > bpf/vmlinux.h.tmp
	mv -f bpf/vmlinux.h.tmp bpf/vmlinux.h

build:
	cargo build --workspace --release --offline

install: build bpf
	$(MAKE) --no-print-directory install-files

install-files:
	$(INSTALL) -d -m 0755 "$(DESTDIR)$(BINDIR)" "$(DESTDIR)$(SBINDIR)"
	$(INSTALL) -m 0755 target/release/censorguardd "$(DESTDIR)$(SBINDIR)/censorguardd"
	$(INSTALL) -m 0755 target/release/censorguardctl "$(DESTDIR)$(BINDIR)/censorguardctl"
	$(INSTALL) -m 0755 target/release/censorguard-audit "$(DESTDIR)$(BINDIR)/censorguard-audit"
	$(INSTALL) -m 0755 target/release/censorguard-grpc "$(DESTDIR)$(BINDIR)/censorguard-grpc"
	$(INSTALL) -m 0755 target/release/censorguard-exec "$(DESTDIR)$(BINDIR)/censorguard-exec"
	$(INSTALL) -d -m 0755 "$(DESTDIR)$(AGENTLIBDIR)"
	$(INSTALL) -m 0644 bpf/enforce.bpf.o "$(DESTDIR)$(AGENTLIBDIR)/enforce.bpf.o"
	$(INSTALL) -d -m 0750 "$(DESTDIR)$(SYSCONFDIR)"
	@for cfg in base.yaml standard-dev.yaml high-priv.yaml; do \
		if [ -f "$(DESTDIR)$(SYSCONFDIR)/$$cfg" ]; then \
			printf 'preserving existing %s\n' "$(DESTDIR)$(SYSCONFDIR)/$$cfg"; \
		else \
			$(INSTALL) -m 0640 "config/$$cfg" "$(DESTDIR)$(SYSCONFDIR)/$$cfg"; \
		fi; \
	done
	$(INSTALL) -d -m 0755 "$(DESTDIR)$(SYSTEMDUNITDIR)" "$(DESTDIR)$(SYSUSERSDIR)" "$(DESTDIR)$(TMPFILESDIR)"
	$(INSTALL) -m 0644 packaging/systemd/censorguardd.service "$(DESTDIR)$(SYSTEMDUNITDIR)/censorguardd.service"
	$(INSTALL) -m 0644 packaging/sysusers/censorguard.conf "$(DESTDIR)$(SYSUSERSDIR)/censorguard.conf"
	$(INSTALL) -m 0644 packaging/tmpfiles/censorguard.conf "$(DESTDIR)$(TMPFILESDIR)/censorguard.conf"
	$(INSTALL) -d -m 0755 "$(DESTDIR)$(DOCDIR)" "$(DESTDIR)$(LICENSEDIR)"
	$(INSTALL) -m 0644 README.md NOTICE "$(DESTDIR)$(DOCDIR)"
	$(INSTALL) -m 0644 docs/*.md "$(DESTDIR)$(DOCDIR)"
	$(INSTALL) -m 0644 LICENSE "$(DESTDIR)$(LICENSEDIR)/LICENSE"

uninstall:
	$(RM) "$(DESTDIR)$(SBINDIR)/censorguardd"
	$(RM) "$(DESTDIR)$(BINDIR)/censorguardctl" "$(DESTDIR)$(BINDIR)/censorguard-audit" "$(DESTDIR)$(BINDIR)/censorguard-grpc" "$(DESTDIR)$(BINDIR)/censorguard-exec"
	$(RM) "$(DESTDIR)$(AGENTLIBDIR)/enforce.bpf.o"
	$(RM) "$(DESTDIR)$(SYSTEMDUNITDIR)/censorguardd.service"
	$(RM) "$(DESTDIR)$(SYSUSERSDIR)/censorguard.conf"
	$(RM) "$(DESTDIR)$(TMPFILESDIR)/censorguard.conf"
	$(RM) "$(DESTDIR)$(DOCDIR)/README.md" "$(DESTDIR)$(DOCDIR)/NOTICE"
	$(RM) "$(DESTDIR)$(DOCDIR)/architecture.md" "$(DESTDIR)$(DOCDIR)/migration-matrix.md"
	$(RM) "$(DESTDIR)$(DOCDIR)/deployment.md" "$(DESTDIR)$(DOCDIR)/refactor-plan.md"
	$(RM) "$(DESTDIR)$(DOCDIR)/verification.md" "$(DESTDIR)$(DOCDIR)/current-runbook.md"
	$(RM) "$(DESTDIR)$(DOCDIR)/deepseek-harness-integration.md" "$(DESTDIR)$(DOCDIR)/wechat-censorguard-article.md"
	$(RM) "$(DESTDIR)$(DOCDIR)/操作手册.md"
	$(RM) "$(DESTDIR)$(LICENSEDIR)/LICENSE"
	-rmdir "$(DESTDIR)$(AGENTLIBDIR)" "$(DESTDIR)$(DOCDIR)" "$(DESTDIR)$(LICENSEDIR)" 2>/dev/null
	@printf 'preserved configuration directory: %s\n' "$(DESTDIR)$(SYSCONFDIR)"

package: build bpf
	./scripts/package.sh "$(VERSION)" "$(ARCH)" "$(DISTDIR)"

startup-test: build bpf
	./scripts/test/startup-test.sh

integration-install: build bpf
	./scripts/test/integration-install.sh

integration-basic: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-basic.sh

integration-file-deny: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-file-deny.sh

integration-file-ops: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-file-ops.sh

integration-file-extended: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-file-extended.sh

integration-deep-directory: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-deep-directory.sh

integration-file-stress: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-file-stress.sh

integration-daemon: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-daemon.sh

integration-event-pressure: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-event-pressure.sh

integration-spawn: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-spawn.sh

integration-launcher-race: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-launcher-race.sh

integration-policy-revision: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-policy-revision.sh

integration-exec-domains: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-exec-domains.sh

integration-execveat: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-execveat.sh

integration-reload: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-reload.sh

integration-bind: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-bind.sh

integration-network: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-network.sh

integration-ipv6: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-ipv6.sh

integration-self-protect: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-self-protect.sh

integration-auth: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-auth.sh

integration-sighup: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-sighup.sh

integration-shutdown: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-shutdown.sh

integration-inspection: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-inspection.sh

integration-draining: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-draining.sh

integration-pidtree: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-pidtree.sh

integration-generation: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-generation.sh

integration-pid-churn: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-pid-churn.sh

integration-pid-capacity: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-pid-capacity.sh

integration-rollback: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-rollback.sh

integration-policy-sharing: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-policy-sharing.sh

integration-dns: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-dns.sh

integration-dns-refresh: bpf
	cargo build --workspace --offline
	sudo ./scripts/test/integration-dns-refresh.sh

integration: integration-basic integration-install integration-file-deny integration-file-ops integration-file-extended integration-deep-directory integration-file-stress integration-daemon integration-event-pressure integration-spawn integration-launcher-race integration-policy-revision integration-exec-domains integration-execveat integration-reload integration-bind integration-network integration-ipv6 integration-self-protect integration-auth integration-sighup integration-shutdown integration-inspection integration-draining integration-pidtree integration-generation integration-pid-churn integration-pid-capacity integration-rollback integration-policy-sharing integration-dns integration-dns-refresh

clean:
	cargo clean
	$(RM) bpf/enforce.bpf.o bpf/vmlinux.h
