import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import Control from "./components/Control";
import Pin from "./components/Pin";

// One frontend bundle serves every window — branch on the window label.
const label = getCurrentWebviewWindow().label;

export default function App() {
  if (label.startsWith("pin-")) return <Pin />;
  return <Control />;
}
