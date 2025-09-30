PKGNAME = ccusage-statusline-rs
VERSION = 1.0.0
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