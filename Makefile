PKGNAME = ccusage-statusline-rs
VERSION = $(shell grep -Po '^version = "\K[^"]+' Cargo.toml)
TARBALL = $(PKGNAME)-$(VERSION).tar.xz

.PHONY: all tarball package install clean

all: package

tarball:
	git archive --format=tar --prefix=$(PKGNAME)-$(VERSION)/ HEAD | xz -c > $(TARBALL)

package: tarball
	makepkg -si --noconfirm

install:
	makepkg -si --noconfirm

clean:
	rm -f $(TARBALL)
	rm -rf $(PKGNAME)-$(VERSION)/
	rm -rf pkg/
	rm -f *.pkg.tar.zst