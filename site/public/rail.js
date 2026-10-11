import { $, $$ } from "./dom.js";

let railLinks = [];

function keepInRail(link) {
  const rail = link.parentElement;
  if (!rail || rail.scrollHeight <= rail.clientHeight + 1) return;
  const top = link.offsetTop;
  const bottom = top + link.offsetHeight;
  if (top < rail.scrollTop) rail.scrollTop = top;
  else if (bottom > rail.scrollTop + rail.clientHeight) rail.scrollTop = bottom - rail.clientHeight;
}

function markRail() {
  let current = railLinks.length ? railLinks[0] : null;
  for (const pair of railLinks) {
    if (pair.section.getBoundingClientRect().top <= innerHeight * 0.25) current = pair;
  }
  if (railLinks.length && innerHeight + scrollY >= document.documentElement.scrollHeight - 2) {
    current = railLinks[railLinks.length - 1];
  }
  for (const pair of railLinks) pair.link.classList.toggle("on", pair === current);
  if (current) keepInRail(current.link);
}

export function railSpy() {
  railLinks = [];
  const rail = $(".rail") || $(".onpage");
  if (!rail) return;
  for (const link of $$('a[href^="#"]', rail)) {
    const section = document.getElementById(decodeURIComponent(link.getAttribute("href").slice(1)));
    if (section) railLinks.push({ section, link });
  }
  markRail();
}

addEventListener("scroll", markRail, { passive: true });
addEventListener("resize", markRail, { passive: true });
