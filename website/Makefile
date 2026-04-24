DIST    := dist
PIGEON  := ../etc/aimx-pigeon.svg
INSTALL := ../install.sh

.PHONY: build serve clean

build:
	rm -rf $(DIST)
	mdbook build .
	mkdir -p $(DIST)/assets
	cp -R public/. $(DIST)/
	cp $(PIGEON) $(DIST)/assets/aimx-pigeon.svg
	# Publish the installer at https://aimx.email/install.sh so the
	# curl-to-shell one-liner has a canonical URL. GitHub Pages serves
	# files under their on-disk extension; `.sh` resolves to
	# `application/x-sh` by default, which is correct for `curl | sh`.
	cp $(INSTALL) $(DIST)/install.sh

serve: build
	@echo ""
	@echo "  Serving dist/ at http://localhost:8000/"
	@echo "  Book:           http://localhost:8000/book/"
	@echo ""
	cd $(DIST) && python3 -m http.server 8000

clean:
	rm -rf $(DIST)
