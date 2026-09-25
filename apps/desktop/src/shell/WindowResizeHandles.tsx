import { useEffect, useState } from "react";

import { loadAppBuildInfo } from "../updates/app-updater";

type ResizeDirection =
  | "East"
  | "North"
  | "NorthEast"
  | "NorthWest"
  | "South"
  | "SouthEast"
  | "SouthWest"
  | "West";

const RESIZE_HANDLES: ReadonlyArray<{ direction: ResizeDirection; placement: string }> = [
  { direction: "North", placement: "north" },
  { direction: "South", placement: "south" },
  { direction: "West", placement: "west" },
  { direction: "East", placement: "east" },
  { direction: "NorthWest", placement: "north-west" },
  { direction: "NorthEast", placement: "north-east" },
  { direction: "SouthWest", placement: "south-west" },
  { direction: "SouthEast", placement: "south-east" },
];

/**
 * Undecorated windows on Linux/GTK have no window-manager border to grab, so the
 * frameless shell draws its own thin resize strips along the window edges.
 * Windows and macOS keep their native decorations and render nothing here, and
 * the browser preview has no window to resize.
 */
export function WindowResizeHandles({ maximized }: { maximized: boolean }) {
  const [platform, setPlatform] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    void loadAppBuildInfo()
      .then((info) => {
        if (!disposed) setPlatform(info.platform);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
    };
  }, []);

  if (maximized || platform !== "linux") return null;

  const startResize = async (direction: ResizeDirection) => {
    try {
      // Static import cannot work here: the Tauri window API only resolves
      // inside the desktop shell and throws in the browser preview.
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().startResizeDragging(direction);
    } catch {
      // Window resizing is unavailable outside the desktop shell.
    }
  };

  return (
    <div className="window-resize-handles" aria-hidden="true">
      {RESIZE_HANDLES.map(({ direction, placement }) => (
        <div
          key={direction}
          className={`window-resize-handle window-resize-handle-${placement}`}
          data-resize-direction={direction}
          onPointerDown={(event) => {
            if (event.button !== 0) return;
            event.preventDefault();
            void startResize(direction);
          }}
        />
      ))}
    </div>
  );
}
