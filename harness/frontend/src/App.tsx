import { useEffect, useState } from "react";
import BenchHeader from "./components/BenchHeader";
import { api } from "./lib/api";
import Layers from "./pages/Layers";
import Live from "./pages/Live";
import Overview from "./pages/Overview";
import Runs from "./pages/Runs";

// Hash-based routing: #/  #/layers  #/live  #/runs
function useHashRoute(): string {
  const [route, setRoute] = useState(() => window.location.hash.replace(/^#/, "") || "/");
  useEffect(() => {
    const onChange = () => setRoute(window.location.hash.replace(/^#/, "") || "/");
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return route;
}

export default function App() {
  const route = useHashRoute();
  const [backend, setBackend] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .telemetry()
        .then((t) => alive && setBackend(t.backend))
        .catch(() => {
          /* backend label is cosmetic */
        });
    void load();
    const t = setInterval(load, 10_000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  const page = route.startsWith("/layers") ? (
    <Layers />
  ) : route.startsWith("/live") ? (
    <Live />
  ) : route.startsWith("/runs") ? (
    <Runs />
  ) : (
    <Overview />
  );

  return (
    <div className="min-h-screen">
      <BenchHeader route={route} backend={backend} />
      <main className="mx-auto max-w-6xl space-y-6 px-4 py-6 sm:px-6">{page}</main>
    </div>
  );
}
