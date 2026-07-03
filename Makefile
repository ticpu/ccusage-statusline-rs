PKGNAME = ccusage-statusline-rs
VERSION = $(shell grep -Po '^version = "\K[^"]+' Cargo.toml)
TARBALL = $(PKGNAME)-$(VERSION).tar.xz

.PHONY: all tarball package install clean

all: package

tarball:
	git archive --format=tar --prefix=$(PKGNAME)-$(VERSION)/ HEAD > $(PKGNAME)-$(VERSION).tar
	@if [ -z "$$(git ls-files Cargo.lock)" ]; then \
		echo "Cargo.lock untracked; generating and injecting..."; \
		cargo generate-lockfile; \
		tar -rf $(PKGNAME)-$(VERSION).tar --transform='s,^,$(PKGNAME)-$(VERSION)/,' Cargo.lock; \
	fi
	xz -c $(PKGNAME)-$(VERSION).tar > $(TARBALL)
	@rm -f $(PKGNAME)-$(VERSION).tar
	@echo "Created $(TARBALL)"

package: tarball
	@echo "Preparing PKGBUILD for local build..."
	@cp PKGBUILD PKGBUILD.bak
	@sed -i 's|source=("https://github.com/ticpu/$$pkgname/releases/download/v$$pkgver/$$pkgname-$$pkgver.tar.xz")|source=("$$pkgname-$$pkgver.tar.xz")|' PKGBUILD
	makepkg -si --noconfirm
	@mv PKGBUILD.bak PKGBUILD

install:
	makepkg -si --noconfirm

clean:
	rm -f $(TARBALL)
	rm -rf $(PKGNAME)-$(VERSION)/
	rm -rf pkg/
	rm -f *.pkg.tar.zst
	rm -f PKGBUILD.bak