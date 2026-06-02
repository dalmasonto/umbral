import { useEffect } from "react";
import { usePlayground } from "./state/store";
import { loadHistory } from "./state/history";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "./components/ui/sidebar";
import { Button } from "./components/ui/button";
import { Separator } from "./components/ui/separator";
import { TooltipProvider } from "./components/ui/tooltip";

export function App() {
  const spec = usePlayground((s) => s.spec);
  const specError = usePlayground((s) => s.specError);
  const loadSpec = usePlayground((s) => s.loadSpec);

  useEffect(() => {
    void loadSpec();
  }, [loadSpec]);

  useEffect(() => {
    usePlayground.setState({ history: loadHistory() });
  }, []);

  return (
    <TooltipProvider delayDuration={0}>
      <SidebarProvider>
        <Sidebar collapsible="offcanvas">
          <SidebarHeader>
            <h2 className="px-2 py-1 text-sm font-semibold tracking-wide">
              umbra playground
            </h2>
          </SidebarHeader>
          <Separator />
          <SidebarContent>
            {/* EndpointTree lands here next iteration */}
            <div className="p-4 text-xs text-muted-foreground">
              Endpoint tree placeholder
            </div>
          </SidebarContent>
          <Separator />
          <SidebarFooter>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void loadSpec()}
              className="w-full justify-start"
            >
              Reload spec
            </Button>
          </SidebarFooter>
        </Sidebar>
        <SidebarInset>
          <header className="flex h-14 items-center gap-2 border-b border-border px-4">
            <SidebarTrigger />
            <h1 className="text-sm font-medium">
              {spec?.info?.title ?? "playground"}
              {spec?.info?.version && (
                <span className="ml-2 text-xs text-muted-foreground">
                  v{spec.info.version}
                </span>
              )}
            </h1>
            {specError && (
              <span className="ml-4 text-xs text-destructive">
                spec error: {specError}
              </span>
            )}
          </header>
          <div className="grid flex-1 grid-cols-2 gap-0">
            {/* RequestBuilder placeholder */}
            <section className="border-r border-border p-4 text-xs text-muted-foreground">
              Request builder placeholder
            </section>
            {/* ResponseViewer placeholder */}
            <section className="p-4 text-xs text-muted-foreground">
              Response viewer placeholder
            </section>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  );
}

export default App;
