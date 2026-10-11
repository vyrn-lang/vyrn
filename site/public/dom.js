export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

export function pinWidth(btn) {
  btn.style.minWidth = Math.ceil(btn.getBoundingClientRect().width) + "px";
}
