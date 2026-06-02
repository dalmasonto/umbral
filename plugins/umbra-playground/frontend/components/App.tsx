import { useEffect } from "react";
import { usePlayground } from "../state/store";
import { loadHistory } from "../state/history";
import { Header } from "./Header";
import { EndpointTree } from "./EndpointTree";
import { RequestBuilder } from "./RequestBuilder";
import { ResponseViewer } from "./ResponseViewer";
import { ErrorBanner } from "./ErrorBanner";

export function App() {
  const loadSpec = usePlayground((s) => s.loadSpec);
  const specError = usePlayground((s) => s.specError);

  useEffect(() => {
    void loadSpec();
  }, [loadSpec]);

  useEffect(() => {
    usePlayground.setState({ history: loadHistory() });
  }, []);

  return (
    <div className="h-screen flex flex-col bg-slate-950 text-slate-300">
      <Header />
      {specError && <ErrorBanner message={specError} onRetry={() => void loadSpec()} />}
      <div className="flex-1 grid grid-cols-[240px_1fr_1fr] overflow-hidden">
        <aside className="border-r border-slate-800 overflow-y-auto">
          <EndpointTree />
        </aside>
        <main className="border-r border-slate-800 overflow-hidden flex flex-col">
          <RequestBuilder />
        </main>
        <section className="overflow-hidden flex flex-col">
          <ResponseViewer />
        </section>
      </div>
    </div>
  );
}
