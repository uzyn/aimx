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

  function initCopyButtons() {
    var buttons = document.querySelectorAll('.terminal-copy[data-copy]');
    for (var i = 0; i < buttons.length; i++) {
      (function (btn) {
        var label = btn.querySelector('.terminal-copy-label');
        var defaultText = label ? label.textContent : 'Copy';
        var resetTimer = null;
        btn.addEventListener('click', function () {
          var text = btn.getAttribute('data-copy') || '';
          var done = function (ok) {
            if (!label) return;
            label.textContent = ok ? 'Copied' : 'Failed';
            btn.classList.toggle('is-copied', ok);
            if (resetTimer) clearTimeout(resetTimer);
            resetTimer = setTimeout(function () {
              label.textContent = defaultText;
              btn.classList.remove('is-copied');
            }, 1600);
          };
          if (navigator.clipboard && navigator.clipboard.writeText) {
            navigator.clipboard.writeText(text).then(function () { done(true); }, function () { done(false); });
          } else {
            try {
              var ta = document.createElement('textarea');
              ta.value = text;
              ta.setAttribute('readonly', '');
              ta.style.position = 'absolute';
              ta.style.left = '-9999px';
              document.body.appendChild(ta);
              ta.select();
              var ok = document.execCommand && document.execCommand('copy');
              document.body.removeChild(ta);
              done(!!ok);
            } catch (e) {
              done(false);
            }
          }
        });
      })(buttons[i]);
    }
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
    initCopyButtons();
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
