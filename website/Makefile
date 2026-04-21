DIST   := dist
PIGEON := ../etc/aimx-pigeon.svg

.PHONY: build serve clean

build:
	rm -rf $(DIST)
	mdbook build .
	mkdir -p $(DIST)/assets
	cp -R public/. $(DIST)/
	cp $(PIGEON) $(DIST)/assets/aimx-pigeon.svg

serve: build
	@echo ""
	@echo "  Serving dist/ at http://localhost:8000/"
	@echo "  Book:           http://localhost:8000/book/"
	@echo ""
	cd $(DIST) && python3 -m http.server 8000

clean:
	rm -rf $(DIST)
