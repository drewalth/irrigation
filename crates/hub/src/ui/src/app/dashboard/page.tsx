import { SectionCards } from "@/components/section-cards";
import { ZoneCards } from "@/components/zone-cards";
import { MoistureChart } from "@/components/chart-area-interactive";
import { WateringEventsTable } from "@/components/watering-events-table";
import { EventLog } from "@/components/event-log";
import { ConnectionBanner } from "@/components/connection-banner";
import { AppSidebar } from "@/components/app-sidebar";
import { SiteHeader } from "@/components/site-header";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useStatus } from "@/hooks/use-api";

interface DashboardPageProps {
  currentPage: string;
  onNavigate: (page: string) => void;
}

function EventsTabs({ mode }: { mode?: "auto" | "monitor" }) {
  const isMonitor = mode === "monitor";
  return (
    <Tabs defaultValue={isMonitor ? "log" : "events"}>
      <TabsList>
        {!isMonitor && (
          <TabsTrigger value="events">Watering Events</TabsTrigger>
        )}
        <TabsTrigger value="log">System Log</TabsTrigger>
      </TabsList>
      {!isMonitor && (
        <TabsContent value="events">
          <WateringEventsTable />
        </TabsContent>
      )}
      <TabsContent value="log">
        <EventLog />
      </TabsContent>
    </Tabs>
  );
}

export default function Page({ currentPage, onNavigate }: DashboardPageProps) {
  const { data: status, error: statusError } = useStatus();
  return (
    <SidebarProvider
      style={
        {
          "--sidebar-width": "calc(var(--spacing) * 72)",
          "--header-height": "calc(var(--spacing) * 12)",
        } as any
      }
    >
      <AppSidebar
        currentPage={currentPage}
        onNavigate={onNavigate}
        variant="inset"
      />
      <SidebarInset>
        <SiteHeader currentPage={currentPage} />
        <ConnectionBanner error={statusError} />
        <div className="flex flex-1 flex-col">
          <div className="@container/main flex flex-1 flex-col gap-2">
            <div className="flex flex-col gap-4 py-4 md:gap-6 md:py-6">
              {currentPage === "overview" && (
                <>
                  <SectionCards />
                  <ZoneCards />
                  <div className="px-4 lg:px-6">
                    <MoistureChart />
                  </div>
                  <div className="px-4 lg:px-6">
                    <EventsTabs mode={status?.mode} />
                  </div>
                </>
              )}
              {currentPage === "zones" && <ZoneCards />}
              {currentPage === "sensors" && (
                <div className="px-4 lg:px-6">
                  <MoistureChart />
                </div>
              )}
              {currentPage === "events" && (
                <div className="px-4 lg:px-6">
                  <EventsTabs mode={status?.mode} />
                </div>
              )}
            </div>
          </div>
        </div>
      </SidebarInset>
    </SidebarProvider>
  );
}
