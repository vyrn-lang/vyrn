const KEY = "vyrn.theme";
const root = document.documentElement;

const ORDER = ["", "light", "dark"];
const WORD = { "": "System", light: "Light", dark: "Dark" };

function stored() {
  try {
    const v = localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : "";
  } catch (err) {
    return "";
  }
}

function apply(choice) {
  if (choice) root.setAttribute("data-theme", choice);
  else root.removeAttribute("data-theme");
}

function next(choice) {
  return ORDER[(ORDER.indexOf(choice) + 1) % ORDER.length];
}

function mark(choice) {
  const buttons = document.querySelectorAll("[data-theme-cycle]");
  for (let i = 0; i < buttons.length; i += 1) {
    buttons[i].textContent = WORD[choice];
    buttons[i].setAttribute("aria-label", "Theme: " + WORD[choice] + ". Press to use " + WORD[next(choice)] + ".");
  }
}

document.addEventListener("click", function (e) {
  const btn = e.target.closest ? e.target.closest("[data-theme-cycle]") : null;
  if (!btn) return;
  const choice = next(root.getAttribute("data-theme") || "");
  try {
    if (choice) localStorage.setItem(KEY, choice);
    else localStorage.removeItem(KEY);
  } catch (err) {}
  apply(choice);
  mark(choice);
});

mark(stored());
