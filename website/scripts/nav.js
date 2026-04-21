(function () {
  function init() {
    var menuBar = document.getElementById('menu-bar');
    if (!menuBar) return;

    var title = menuBar.querySelector('.menu-title');
    if (title && !title.querySelector('a')) {
      var text = title.textContent.trim();
      title.textContent = '';
      var a = document.createElement('a');
      a.href = '/';
      a.textContent = text;
      a.className = 'menu-title-home';
      title.appendChild(a);
    }

    if (menuBar.querySelector('.aimx-topnav')) return;

    var nav = document.createElement('nav');
    nav.className = 'aimx-topnav';
    nav.setAttribute('aria-label', 'Primary');
    nav.innerHTML =
      '<a href="/">home</a>' +
      '<a href="/book/" class="active">book</a>' +
      '<a href="https://github.com/uzyn/aimx" target="_blank" rel="noopener">code</a>';

    var rightButtons = menuBar.querySelector('.right-buttons');
    if (rightButtons) {
      menuBar.insertBefore(nav, rightButtons);
    } else {
      menuBar.appendChild(nav);
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
