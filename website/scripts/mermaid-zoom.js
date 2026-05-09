// Click-to-zoom for mdBook mermaid diagrams.
// mdbook-mermaid renders each ```mermaid block into a <div class="mermaid">
// containing an inline SVG. Diagrams render small inside the book's text
// column, so this script wires each block to open its SVG in a new tab
// where the browser's native zoom (Cmd/Ctrl + scroll, pinch) takes over.
(function () {
  function attach(block) {
    if (block.dataset.zoomBound === '1') return;
    block.dataset.zoomBound = '1';
    block.classList.add('mermaid-zoomable');
    block.title = 'Click to open in a new tab to zoom';
    block.addEventListener('click', function (e) {
      e.preventDefault();
      openInNewTab(block);
    });
  }

  function openInNewTab(block) {
    var svg = block.querySelector('svg');
    if (!svg) return;
    var clone = svg.cloneNode(true);
    if (!clone.getAttribute('xmlns')) clone.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
    if (!clone.getAttribute('xmlns:xlink')) clone.setAttribute('xmlns:xlink', 'http://www.w3.org/1999/xlink');
    // Drop the inline max-width/height mermaid sets so the SVG fills the
    // viewport. The viewBox preserves geometry; native zoom works from there.
    clone.removeAttribute('style');
    clone.setAttribute('width', '100%');
    clone.setAttribute('height', '100%');
    clone.setAttribute('preserveAspectRatio', 'xMidYMid meet');

    var bg = getComputedStyle(document.body).backgroundColor || '#0D1117';
    var serial = new XMLSerializer().serializeToString(clone);
    var title = (document.title || 'Diagram') + ' — diagram';

    var html =
      '<!doctype html><html><head><meta charset="utf-8">' +
      '<meta name="viewport" content="width=device-width,initial-scale=1">' +
      '<title>' + title.replace(/[<>&"]/g, '') + '</title>' +
      '<style>' +
      'html,body{margin:0;padding:0;height:100%;background:' + bg + ';' +
      'display:flex;align-items:center;justify-content:center;' +
      'font-family:system-ui,sans-serif;}' +
      'svg{max-width:100%;max-height:100%;width:auto;height:auto;padding:24px;box-sizing:border-box;}' +
      '</style></head><body>' + serial + '</body></html>';

    var w = window.open('', '_blank');
    if (!w) return;
    w.document.open();
    w.document.write(html);
    w.document.close();
  }

  function scan() {
    var blocks = document.querySelectorAll('.mermaid');
    blocks.forEach(function (block) {
      if (block.querySelector('svg')) {
        attach(block);
      }
    });
  }

  function start() {
    // mermaid renders asynchronously after init; an observer catches each
    // SVG as it lands. One scan up front handles already-rendered ones.
    scan();
    var root = document.querySelector('main') || document.body;
    if (!root) return;
    var observer = new MutationObserver(scan);
    observer.observe(root, { childList: true, subtree: true });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', start);
  } else {
    start();
  }
})();
