PKGNAME = ccusage-statusline-rs
VERSION = $(shell grep -Po '^version = "\K[^"]+' Cargo.toml)
TARBALL = $(PKGNAME)-$(VERSION).tar.xz

.PHONY: all tarball package install clean

all: package

tarball:
	@echo "Generating Cargo.lock for release..."
	@cargo generate-lockfile
	git archive --format=tar --prefix=$(PKGNAME)-$(VERSION)/ HEAD > $(PKGNAME)-$(VERSION).tar
	@echo "Adding Cargo.lock to tarball..."
	tar -rf $(PKGNAME)-$(VERSION).tar --transform='s,^,$(PKGNAME)-$(VERSION)/,' Cargo.lock
	xz -c $(PKGNAME)-$(VERSION).tar > $(TARBALL)
	@rm -f $(PKGNAME)-$(VERSION).tar Cargo.lock
	@echo "Created $(TARBALL)"

package: tarball
	@echo "Preparing PKGBUILD for local build..."
	@cp PKGBUILD PKGBUILD.bak
	@sed -i 's|source=("$$pkgname-$$pkgver.tar.xz::https://github.com/ticpu/$$pkgname/archive/v$$pkgver.tar.xz")|source=("$$pkgname-$$pkgver.tar.xz")|' PKGBUILD
	makepkg -si --noconfirm
	@mv PKGBUILD.bak PKGBUILD

install:
	makepkg -si --noconfirm

clean:
	rm -f $(TARBALL)
	rm -rf $(PKGNAME)-$(VERSION)/
	rm -rf pkg/
	rm -f *.pkg.tar.zst
	rm -f Cargo.lock
	rm -f PKGBUILD.bak