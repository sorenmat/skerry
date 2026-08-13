const commands = {
  macos: `brew tap sorenmat/skerry
brew trust sorenmat/skerry
brew install --cask skerry`,
  linux: `brew tap sorenmat/skerry
brew install skerry`,
};

const command = document.querySelector("[data-install-command]");
const copyButton = document.querySelector("[data-copy]");

document.querySelectorAll("[data-platform]").forEach((button) => {
  button.addEventListener("click", () => {
    document.querySelectorAll("[data-platform]").forEach((candidate) => {
      candidate.setAttribute("aria-pressed", String(candidate === button));
    });
    command.textContent = commands[button.dataset.platform];
    copyButton.textContent = "Copy";
  });
});

copyButton.addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText(command.textContent);
    copyButton.textContent = "Copied";
  } catch {
    const range = document.createRange();
    range.selectNodeContents(command);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    copyButton.textContent = "Selected";
  }
});
