import { Separator } from "@/components/ui/separator";
import { SidebarTrigger } from "@/components/ui/sidebar";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
} from "@/components/ui/breadcrumb";

const pageLabels: Record<string, string> = {
  overview: "Overview",
  zones: "Zones",
  sensors: "Sensors",
  events: "Events",
};

interface SiteHeaderProps {
  currentPage: string;
}

export function SiteHeader({ currentPage }: SiteHeaderProps) {
  return (
    <header className="flex h-(--header-height) shrink-0 items-center gap-2 border-b transition-[width,height] ease-linear group-has-data-[collapsible=icon]/sidebar-wrapper:h-(--header-height)">
      <div className="flex w-full items-center gap-1 px-4 lg:gap-2 lg:px-6">
        <SidebarTrigger className="-ml-1" />
        <Separator
          orientation="vertical"
          className="mx-2 data-[orientation=vertical]:h-4"
        />
        <Breadcrumb>
          <BreadcrumbList>
            <BreadcrumbItem>
              <BreadcrumbPage>
                {pageLabels[currentPage] ?? currentPage}
              </BreadcrumbPage>
            </BreadcrumbItem>
          </BreadcrumbList>
        </Breadcrumb>

        {/* <div className="ml-auto flex items-center gap-2">
          {status && (
            <Badge variant="outline" className="text-xs">
              {formatUptime(uptimeSecs)}
            </Badge>
          )}
          <Badge
            variant={mqttConnected ? "default" : "destructive"}
            className="text-xs"
          >
            <span
              className={`inline-block size-1.5 rounded-full ${
                mqttConnected ? "bg-green-300" : "bg-red-300"
              }`}
            />
            {mqttConnected ? "Connected" : "Disconnected"}
          </Badge>
        </div> */}
      </div>
    </header>
  );
}
