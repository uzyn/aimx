// aimx theme toggle — three states: auto (system), light, dark.
// Persisted under localStorage["aimx-theme"], distinct from mdBook's key.
(function () {
  var KEY = 'aimx-theme';

  function setMode(mode) {
    if (mode === 'auto' || !mode) {
      document.documentElement.removeAttribute('data-theme');
      try { localStorage.removeItem(KEY); } catch (e) {}
      mode = 'auto';
    } else {
      document.documentElement.setAttribute('data-theme', mode);
      try { localStorage.setItem(KEY, mode); } catch (e) {}
    }
    var btns = document.querySelectorAll('.theme-toggle button');
    for (var i = 0; i < btns.length; i++) {
      btns[i].setAttribute('aria-pressed', btns[i].getAttribute('data-mode') === mode ? 'true' : 'false');
    }
  }

  function init() {
    var stored = null;
    try { stored = localStorage.getItem(KEY); } catch (e) {}
    setMode(stored || 'auto');
    var btns = document.querySelectorAll('.theme-toggle button');
    for (var i = 0; i < btns.length; i++) {
      (function (btn) {
        btn.addEventListener('click', function () { setMode(btn.getAttribute('data-mode')); });
      })(btns[i]);
    }
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
