const { invoke } = window.__TAURI__.core;

const folderList = document.getElementById("folder-list");
const folderEmpty = document.getElementById("folder-empty");
const peerList = document.getElementById("peer-list");
const peerEmpty = document.getElementById("peer-empty");
const peerCount = document.getElementById("peer-count");
const addFolderBtn = document.getElementById("add-folder-btn");
const sharedSecretInput = document.getElementById("shared-secret-input");
const saveSecretBtn = document.getElementById("save-secret-btn");
const secretSavedHint = document.getElementById("secret-saved-hint");

async function refreshFolders() {
  const folders = await invoke("get_watched_folders");
  folderList.innerHTML = "";
  folderEmpty.hidden = folders.length > 0;

  for (const folder of folders) {
    const li = document.createElement("li");

    const path = document.createElement("span");
    path.className = "path";
    path.textContent = folder;

    const removeBtn = document.createElement("button");
    removeBtn.className = "secondary";
    removeBtn.textContent = "削除";
    removeBtn.addEventListener("click", async () => {
      await invoke("remove_watched_folder", { path: folder });
      await refreshFolders();
    });

    li.append(path, removeBtn);
    folderList.append(li);
  }
}

async function refreshPeers() {
  const peers = await invoke("get_peers");
  peerList.innerHTML = "";
  peerEmpty.hidden = peers.length > 0;
  peerCount.textContent = String(peers.length);

  for (const peer of peers) {
    const li = document.createElement("li");

    const name = document.createElement("span");
    name.className = "peer-name";
    name.textContent = peer.hostname;

    const addr = document.createElement("span");
    addr.className = "peer-addr";
    addr.textContent = peer.addr;

    li.append(name, addr);
    peerList.append(li);
  }
}

addFolderBtn.addEventListener("click", async () => {
  const path = await invoke("pick_folder");
  if (!path) return;
  await invoke("add_watched_folder", { path });
  await refreshFolders();
});

async function loadSharedSecret() {
  sharedSecretInput.value = await invoke("get_shared_secret");
}

saveSecretBtn.addEventListener("click", async () => {
  const secret = sharedSecretInput.value.trim();
  if (!secret) return;
  await invoke("set_shared_secret", { secret });
  secretSavedHint.hidden = false;
  setTimeout(() => {
    secretSavedHint.hidden = true;
  }, 4000);
});

refreshFolders();
refreshPeers();
loadSharedSecret();
setInterval(refreshPeers, 3000);
