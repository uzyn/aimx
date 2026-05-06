// aimx theme toggle — two states: dark (default) and light.
// Dark is the brand default for this dev-first tool; light is opt-in.
// Persisted under localStorage["aimx-theme"], distinct from mdBook's key.
(function () {
  var KEY = 'aimx-theme';

  function setMode(mode) {
    if (mode === 'light') {
      document.documentElement.setAttribute('data-theme', 'light');
      try { localStorage.setItem(KEY, 'light'); } catch (e) {}
      mode = 'light';
    } else {
      document.documentElement.removeAttribute('data-theme');
      try { localStorage.removeItem(KEY); } catch (e) {}
      mode = 'dark';
    }
    var btns = document.querySelectorAll('.theme-toggle button');
    for (var i = 0; i < btns.length; i++) {
      btns[i].setAttribute('aria-pressed', btns[i].getAttribute('data-mode') === mode ? 'true' : 'false');
    }
  }

  function init() {
    var stored = null;
    try { stored = localStorage.getItem(KEY); } catch (e) {}
    setMode(stored === 'light' ? 'light' : 'dark');
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
