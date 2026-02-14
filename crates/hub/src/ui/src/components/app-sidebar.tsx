import {
  IconCpu,
  IconDashboard,
  IconDroplet,
  IconGrid3x3,
  IconList,
} from "@tabler/icons-react";

import { useStatus } from "@/hooks/use-api";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar";
import { Badge } from "@/components/ui/badge";
import { useToast } from "@/hooks/use-toast";
import { useEffect } from "preact/hooks";

const navItems = [
  { id: "overview", label: "Overview", icon: IconDashboard },
  { id: "zones", label: "Zones", icon: IconGrid3x3 },
  { id: "sensors", label: "Sensors", icon: IconCpu },
  { id: "events", label: "Events", icon: IconList },
];

interface AppSidebarProps {
  currentPage: string;
  onNavigate: (page: string) => void;
}

export function AppSidebar({
  currentPage,
  onNavigate,
  ...props
}: AppSidebarProps & Record<string, any>) {
  return (
    <Sidebar collapsible="offcanvas" {...props}>
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton className="data-[slot=sidebar-menu-button]:!p-1.5">
              <IconDroplet className="!size-5 text-blue-500" />
              <span className="text-base font-semibold">Irrigation Hub</span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupLabel>Navigation</SidebarGroupLabel>
          <SidebarGroupContent>
            <SidebarMenu>
              {navItems.map((item) => (
                <SidebarMenuItem key={item.id}>
                  <SidebarMenuButton
                    isActive={currentPage === item.id}
                    onClick={() => onNavigate(item.id)}
                  >
                    <item.icon className="!size-4" />
                    <span>{item.label}</span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              ))}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>
      <Footer />
    </Sidebar>
  );
}

const Footer = () => {
  const { data: status } = useStatus();
  const mqttConnected = status?.mqtt_connected ?? false;
  const uptimeSecs = status?.uptime_secs ?? 0;
  const toast = useToast();

  useEffect(() => {
    if (mqttConnected) {
      toast.success("MQTT connected");
    } else {
      toast.error("MQTT disconnected");
    }
  }, [mqttConnected]);

  return (
    <SidebarFooter>
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton className="pointer-events-none">
            <span className="flex items-center justify-start gap-2">
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
              {status && (
                <Badge variant="outline" className="text-xs">
                  {formatUptime(uptimeSecs)}
                </Badge>
              )}
            </span>
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
    </SidebarFooter>
  );
};

function formatUptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return `${d}d ${h}h ${m}m`;
}
