// aimx theme toggle — single icon button. Two states: dark (default) and
// light. Dark is the brand default for this dev-first tool; light is opt-in.
// Persisted under localStorage["aimx-theme"], distinct from mdBook's key.
(function () {
  var KEY = 'aimx-theme';

  function setMode(mode) {
    if (mode === 'light') {
      document.documentElement.setAttribute('data-theme', 'light');
      try { localStorage.setItem(KEY, 'light'); } catch (e) {}
    } else {
      document.documentElement.removeAttribute('data-theme');
      try { localStorage.removeItem(KEY); } catch (e) {}
    }
  }

  function currentMode() {
    return document.documentElement.getAttribute('data-theme') === 'light' ? 'light' : 'dark';
  }

  function init() {
    var stored = null;
    try { stored = localStorage.getItem(KEY); } catch (e) {}
    setMode(stored === 'light' ? 'light' : 'dark');
    var btn = document.querySelector('.theme-toggle');
    if (btn) {
      btn.addEventListener('click', function () {
        setMode(currentMode() === 'light' ? 'dark' : 'light');
      });
    }
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
