import { useEffect, useMemo, useState } from "react";
import { createHashRouter, Navigate, RouterProvider } from "react-router-dom";
import { Shell } from "@/components/Shell";
import { Spinner } from "@/components/ui";
import { cmd } from "@/lib/ipc";
import { ProcessingProgressProvider } from "@/lib/processingProgress";
import { installTheme } from "@/lib/theme";
import { AskPage } from "@/routes/Ask";
import { Library } from "@/routes/Library";
import { MeetingPage } from "@/routes/Meeting";
import { Onboarding } from "@/routes/Onboarding";
import RecordingSetup from "@/routes/RecordingSetup";
import { SettingsPage } from "@/routes/Settings";
import { VoicesPage } from "@/routes/Voices";
import { ServiceFixture } from "@/routes/ServiceFixture";
import { Popup } from "@/windows/Popup";
import { RecordBar } from "@/windows/RecordBar";

installTheme();

const params = new URLSearchParams(window.location.search);
const windowKind = params.get("window");
const serviceDesktop = params.get("mode") === "service-desktop";
const managedLibrary = serviceDesktop && params.get("managed") === "1";
const installedManagedLibrary = managedLibrary && params.get("installed") === "1";
const hasOnboarding = !serviceDesktop || installedManagedLibrary;

export default function App() {
  if (windowKind === "service-fixture") return <ServiceFixture />;
  if (windowKind === "popup" || windowKind === "recordbar") {
    document.body.classList.add("transparent");
    return windowKind === "popup" ? <Popup /> : <RecordBar serviceDesktop={serviceDesktop} />;
  }
  return <MainApp />;
}

function MainApp() {
  const [onboarded, setOnboarded] = useState<boolean | null>(hasOnboarding ? null : true);
  useEffect(() => {
    // Keep file drops from navigating the browser preview. The desktop Library
    // imports from Tauri's native drop event, which supplies local paths.
    const keepFileDropInApp = (event: DragEvent) => {
      if (event.dataTransfer?.types.includes("Files")) event.preventDefault();
    };
    window.addEventListener("dragover", keepFileDropInApp);
    window.addEventListener("drop", keepFileDropInApp);
    return () => {
      window.removeEventListener("dragover", keepFileDropInApp);
      window.removeEventListener("drop", keepFileDropInApp);
    };
  }, []);
  useEffect(() => {
    if (!hasOnboarding) return;
    cmd.getSettings().then((s) => setOnboarded(s.onboarded), () => setOnboarded(true));
  }, []);
  // Onboarding flips this when it finishes, otherwise "/" would bounce straight back to it.
  const router = useMemo(
    () =>
      createHashRouter([
        ...(hasOnboarding ? [{
          path: "/onboarding",
          element: installedManagedLibrary && onboarded
            ? <Navigate to="/" replace />
            : <Onboarding serviceDesktop={serviceDesktop} onDone={() => setOnboarded(true)} />,
        }] : []),
        {
          path: "/",
          element: onboarded ? <Shell serviceDesktop={serviceDesktop} managedLibrary={managedLibrary} /> : <Navigate to="/onboarding" replace />,
          children: [
            { index: true, element: <Library serviceDesktop={serviceDesktop} /> },
            { path: "recording", element: <RecordingSetup serviceDesktop={serviceDesktop} settingsAvailable={!serviceDesktop || managedLibrary} /> },
            { path: "meeting/:id", element: <MeetingPage serviceDesktop={serviceDesktop} /> },
            ...(serviceDesktop ? [
              ...(managedLibrary ? [{ path: "settings", element: <SettingsPage serviceDesktop /> }] : []),
              { path: "*", element: <Navigate to="/" replace /> },
            ] : [
              { path: "ask", element: <AskPage /> },
              { path: "settings", element: <SettingsPage /> },
              { path: "voices", element: <VoicesPage /> },
            ]),
          ],
        },
      ]),
    [onboarded],
  );
  return <ProcessingProgressProvider>
    {onboarded === null ? <div className="grid h-full place-items-center"><Spinner /></div> : <RouterProvider router={router} />}
  </ProcessingProgressProvider>;
}
