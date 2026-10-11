let menuEl = null;

function closeMenu() {
  if (menuEl) {
    menuEl.remove();
    menuEl = null;
  }
}

export function openMenu(x, y, items) {
  closeMenu();
  menuEl = document.createElement("div");
  menuEl.className = "ctxmenu";
  menuEl.setAttribute("role", "menu");
  for (const it of items) {
    if (it === "-") {
      const hr = document.createElement("div");
      hr.className = "sep";
      menuEl.append(hr);
      continue;
    }
    const b = document.createElement("button");
    b.type = "button";
    b.className = "mi";
    b.setAttribute("role", "menuitem");
    b.textContent = it.label;
    if (it.keys) {
      const k = document.createElement("span");
      k.className = "mk";
      k.textContent = it.keys;
      b.append(k);
    }
    b.addEventListener("click", () => {
      closeMenu();
      it.act();
    });
    menuEl.append(b);
  }
  document.body.append(menuEl);
  const r = menuEl.getBoundingClientRect();
  menuEl.style.left = Math.min(x, innerWidth - r.width - 6) + "px";
  menuEl.style.top = Math.min(y, innerHeight - r.height - 6) + "px";
}

addEventListener("pointerdown", (e) => {
  if (menuEl && !menuEl.contains(e.target)) closeMenu();
});
addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeMenu();
});
