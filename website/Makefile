DIST      := dist
INSTALL   := ../install.sh
PORTCHECK := ../portcheck.sh

.PHONY: build book pages sitemap llms serve clean

# Full build. mdbook first (writes dist/book/), then static passthrough,
# then sitemap + llms-full regeneration.
build: book pages llms sitemap

book:
	rm -rf $(DIST)
	mdbook build .

pages:
	cp -R public/. $(DIST)/
	mkdir -p $(DIST)/assets
	cp -R assets/. $(DIST)/assets/
	# Publish the installer at https://aimx.email/install.sh so the
	# curl-to-shell one-liner has a canonical URL. GitHub Pages serves
	# files under their on-disk extension; `.sh` resolves to
	# `application/x-sh` by default, which is correct for `curl | sh`.
	cp $(INSTALL) $(DIST)/install.sh
	# Shorter alias for `install.sh --port-check-only` — same trust
	# anchor, fetches install.sh from the same origin and execs it.
	cp $(PORTCHECK) $(DIST)/portcheck.sh
	# Cache-bust local assets on the homepage. Append ?v=<sha-prefix> to
	# /styles/index.css and /scripts/theme.js so the browser refetches
	# when content changes (CDN / GitHub Pages still uses long max-age).
	@HASH_CSS=$$(shasum -a 256 $(DIST)/styles/index.css | cut -c1-8); \
	HASH_JS=$$(shasum -a 256 $(DIST)/scripts/theme.js  | cut -c1-8); \
	sed -i.bak \
	  -e "s|/styles/index\.css\"|/styles/index.css?v=$${HASH_CSS}\"|g" \
	  -e "s|/scripts/theme\.js\"|/scripts/theme.js?v=$${HASH_JS}\"|g" \
	  $(DIST)/index.html
	@rm -f $(DIST)/index.html.bak

# Concatenate every book chapter into a single LLM-ingest file. Order
# follows shell glob (alphabetical), which matches the SUMMARY.md
# naming convention closely enough for crawler purposes.
llms:
	cat ../book/*.md > $(DIST)/llms-full.txt

# Regenerate sitemap.xml by walking every .html file under dist/. The
# hand-authored fallback at public/sitemap.xml is overwritten here so
# the sitemap stays in sync as book chapters are added or renamed.
# 404, toc, and print are mdBook artifacts — excluded from the index.
sitemap:
	@(echo '<?xml version="1.0" encoding="UTF-8"?>'; \
	  echo '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">'; \
	  cd $(DIST) && find . -name '*.html' -type f \
	    | grep -vE '/(404|toc|print)\.html$$' \
	    | LC_ALL=C sort \
	    | sed 's|^\./||; s|/index\.html$$|/|; s|^index\.html$$||; s|^|  <url><loc>https://aimx.email/|; s|$$|</loc></url>|'; \
	  echo '</urlset>') > $(DIST)/sitemap.xml

serve: build
	@echo ""
	@echo "  Serving dist/ at http://localhost:8000/"
	@echo "  Book:           http://localhost:8000/book/"
	@echo ""
	cd $(DIST) && python3 -m http.server 8000

clean:
	rm -rf $(DIST)
