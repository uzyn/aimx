// aimx theme toggle. Defaults to the OS preference; clicking the button
// stores an explicit override under localStorage["aimx-theme"]. While no
// override is stored, the page tracks live system theme changes.
(function () {
  var KEY = 'aimx-theme';
  var mql = window.matchMedia ? window.matchMedia('(prefers-color-scheme: light)') : null;

  function storedMode() {
    try { var v = localStorage.getItem(KEY); return v === 'light' || v === 'dark' ? v : null; } catch (e) { return null; }
  }

  function systemMode() {
    return mql && mql.matches ? 'light' : 'dark';
  }

  function effectiveMode() {
    return storedMode() || systemMode();
  }

  function applyMode(mode) {
    document.documentElement.setAttribute('data-theme', mode === 'light' ? 'light' : 'dark');
  }

  function setExplicit(mode) {
    try { localStorage.setItem(KEY, mode); } catch (e) {}
    applyMode(mode);
  }

  function init() {
    applyMode(effectiveMode());
    var btn = document.querySelector('.theme-toggle');
    if (btn) {
      btn.addEventListener('click', function () {
        setExplicit(effectiveMode() === 'light' ? 'dark' : 'light');
      });
    }
    if (mql) {
      var onChange = function () { if (!storedMode()) applyMode(systemMode()); };
      if (mql.addEventListener) mql.addEventListener('change', onChange);
      else if (mql.addListener) mql.addListener(onChange);
    }
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
