import { useEffect } from "react";
import { usePlayground } from "@/state/store";
import { loadHistory } from "@/state/history";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { TooltipProvider } from "@/components/ui/tooltip";
import { EndpointTree } from "@/components/EndpointTree";
import { RequestBuilder } from "@/components/RequestBuilder";
import { ResponseViewer } from "@/components/ResponseViewer";
import { RotateCcw, Zap } from "lucide-react";

export function App() {
  const spec = usePlayground((s) => s.spec);
  const specError = usePlayground((s) => s.specError);
  const loadingSpec = usePlayground((s) => s.loadingSpec);
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
        <Sidebar collapsible="offcanvas" className="border-r border-border">
          <SidebarHeader className="px-4 py-0 h-[60px] flex flex-row items-center justify-between shrink-0">
            <div className="flex items-center gap-2.5">
              <Zap className="size-5 text-primary" />
              <div className="flex flex-col">
                <span className="text-sm font-semibold tracking-tight leading-tight">
                  umbra playground
                </span>
                <span className="text-[10px] text-muted-foreground leading-tight">
                  {spec?.info?.title ?? "API Explorer"}
                  {spec?.info?.version && (
                    <> <span className="text-muted-foreground/60">v{spec.info.version}</span></>
                  )}
                </span>
              </div>
            </div>
          </SidebarHeader>
          <Separator />
          <SidebarContent className="p-0">
            <EndpointTree />
          </SidebarContent>
          <Separator />
          <SidebarFooter className="p-2">
            <div className="flex items-center justify-between px-2 py-1">
              <span className="text-[10px] text-muted-foreground">
                {loadingSpec
                  ? "Loading…"
                  : specError
                    ? "Spec error"
                    : spec
                      ? `${Object.keys(spec.paths ?? {}).length} paths`
                      : "No spec"}
              </span>
              <Button
                variant="ghost"
                size="icon-xs"
                onClick={() => void loadSpec()}
                title="Reload spec"
                className="text-muted-foreground hover:text-foreground"
              >
                <RotateCcw className="size-3" />
              </Button>
            </div>
          </SidebarFooter>
        </Sidebar>

        <SidebarInset>
          <header className="flex h-[60px] items-center gap-3 border-b border-border px-4 bg-card/50 shrink-0">
            <SidebarTrigger />
            <Separator orientation="vertical" className="h-5" />
            <div className="flex items-baseline gap-2 flex-1">
              <h1 className="text-sm font-medium">
                {spec?.info?.title ?? "playground"}
              </h1>
              {spec?.info?.version && (
                <span className="text-xs text-muted-foreground">
                  v{spec.info.version}
                </span>
              )}
            </div>
            {specError && (
              <span className="text-xs text-destructive font-mono">
                {specError}
              </span>
            )}
          </header>

          <div className="grid flex-1 grid-cols-2 gap-0 overflow-hidden">
            <section className="border-r border-border overflow-hidden flex flex-col">
              <RequestBuilder />
            </section>
            <section className="overflow-hidden flex flex-col">
              <ResponseViewer />
            </section>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  );
}

export default App;
