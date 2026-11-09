import { contextBridge, ipcRenderer, webUtils } from "electron";
import type { HostState, LibraryEntry, WorkshopItem, RinkaApi } from "../shared/api";

const api: RinkaApi = {
  appInfo: () => ipcRenderer.invoke("app:info"),
  appSettings: () => ipcRenderer.invoke("settings:get"),
  setAppSettings: (patch) => ipcRenderer.invoke("settings:set", patch),
  listProjects: () => ipcRenderer.invoke("library:list"),
  createColor: (title, color) => ipcRenderer.invoke("library:create-color", title, color),
  importFiles: (paths, kind) => ipcRenderer.invoke("library:import-files", paths, kind),
  saveProject: (id, project) => ipcRenderer.invoke("library:save", id, project),
  deleteProject: (id) => ipcRenderer.invoke("library:delete", id),
  revealLibrary: () => ipcRenderer.invoke("library:reveal"),
  scanWorkshop: () => ipcRenderer.invoke("workshop:scan"),
  locateWorkshop: () => ipcRenderer.invoke("workshop:locate"),
  resetWorkshopFolder: () => ipcRenderer.invoke("workshop:reset-folder"),
  importWorkshopItem: (id, allowPartial) => ipcRenderer.invoke("workshop:import", id, allowPartial),
  onWorkshopItem: (listener) => {
    ipcRenderer.on("workshop:item", (_event, item: WorkshopItem) => listener(item));
  },
  pathForFile: (file) => webUtils.getPathForFile(file),
  onLibraryChanged: (listener) => {
    ipcRenderer.on("library:changed", (_event, entries: LibraryEntry[]) => listener(entries));
  },
  videoFiles: (id) => ipcRenderer.invoke("video:files", id),
  optimizeVideo: (id, options) => ipcRenderer.invoke("video:optimize", id, options),
  cancelOptimize: (id) => ipcRenderer.invoke("video:cancel", id),
  restoreVideo: (id) => ipcRenderer.invoke("video:restore", id),
  onOptimizeProgress: (listener) => {
    const handler = (_event: Electron.IpcRendererEvent, id: string, progress: number) => listener(id, progress);
    ipcRenderer.on("video:progress", handler);
    return () => void ipcRenderer.removeListener("video:progress", handler);
  },

  hostState: () => ipcRenderer.invoke("host:state"),
  startHost: () => ipcRenderer.invoke("host:start"),
  stopHost: () => ipcRenderer.invoke("host:stop"),
  outputs: () => ipcRenderer.invoke("host:outputs"),
  assign: (output, id) => ipcRenderer.invoke("host:assign", output, id),
  unassign: (output) => ipcRenderer.invoke("host:unassign", output),
  onHostState: (listener) => {
    ipcRenderer.on("host:state", (_event, state: HostState) => listener(state));
  },
  setPaused: (paused) => ipcRenderer.invoke("host:pause", paused),
  plasmaDesktop: (showRinka) => ipcRenderer.invoke("host:plasma-desktop", showRinka),

  reportPreview: (report) => ipcRenderer.send("preview:report", report),
};

contextBridge.exposeInMainWorld("rinka", api);
