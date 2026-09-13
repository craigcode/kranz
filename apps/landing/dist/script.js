"use strict";

const expandButton = document.querySelector(".expand-all");
const featureDetails = [...document.querySelectorAll(".feature-list details")];

if (expandButton && featureDetails.length) {
  const syncExpandButton = () => {
    const allOpen = featureDetails.every((detail) => detail.open);
    expandButton.setAttribute("aria-expanded", String(allOpen));
    expandButton.replaceChildren(
      document.createTextNode(allOpen ? "Collapse all " : "Expand all "),
    );
    const icon = document.createElement("span");
    icon.setAttribute("aria-hidden", "true");
    icon.textContent = allOpen ? "−" : "+";
    expandButton.append(icon);
  };

  expandButton.hidden = false;
  expandButton.addEventListener("click", () => {
    const shouldOpen = featureDetails.some((detail) => !detail.open);
    for (const detail of featureDetails) detail.open = shouldOpen;
    syncExpandButton();
  });
  for (const detail of featureDetails) {
    detail.addEventListener("toggle", syncExpandButton);
  }
}
