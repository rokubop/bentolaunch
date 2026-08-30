## Install

Windows 11 only. Both files ship from the same release and must match - the exe
and the extension check each other's version.

### 1. The app

1. Download `bentolaunch.exe` below.
2. Go to `AppData\Local\Programs`. Paste `%LOCALAPPDATA%\Programs` into the
   Explorer address bar to get there.
3. Create a `bentolaunch` folder if you don't already have one.
4. Put `bentolaunch.exe` in it.
5. If you want the program to auto launch at startup: right-click the exe >
   **Show more options** > **Create shortcut**. Then Win+R, `shell:startup`,
   and drop the shortcut in the folder that opens.
6. Double-click the exe to run. The first time, Windows warns about an
   unrecognised app: **More info** > **Run anyway**.
7. Press `` Alt+` `` to activate BentoLaunch.

### 2. Browser

Required for tabs and bookmarks. Chromium for now.

1. Download `bentolaunch-extension.zip` below. Anywhere is fine.
2. Unzip it.
3. Go to `chrome://extensions`, turn on **Developer mode**, then
   **Load unpacked** and pick the unzipped `bentolaunch-extension` folder.
   Developer mode is needed until this is an official extension.
4. Right-click the BentoLaunch tray icon, bottom right of your desktop >
   **Browser** > **Pair a browser...**. It shows six digits.
5. In your browser's extensions menu, top right: find **BentoLaunch bridge**,
   click its three dots > **Options**, enter the six digits >
   **Pair with BentoLaunch**.
