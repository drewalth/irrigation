import {
  IconCpu,
  IconDashboard,
  IconDroplet,
  IconGrid3x3,
  IconList,
} from "@tabler/icons-react"

import { useStatus } from "@/hooks/use-api"
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
} from "@/components/ui/sidebar"

const navItems = [
  { id: "overview", label: "Overview", icon: IconDashboard },
  { id: "zones", label: "Zones", icon: IconGrid3x3 },
  { id: "sensors", label: "Sensors", icon: IconCpu },
  { id: "events", label: "Events", icon: IconList },
]

interface AppSidebarProps {
  currentPage: string
  onNavigate: (page: string) => void
}

export function AppSidebar({ currentPage, onNavigate, ...props }: AppSidebarProps & Record<string, any>) {
  const { data: status } = useStatus()
  const mqttConnected = status?.mqtt_connected ?? false

  return (
    <Sidebar collapsible="offcanvas" {...props}>
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              className="data-[slot=sidebar-menu-button]:!p-1.5"
            >
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

      <SidebarFooter>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton className="pointer-events-none">
              <span
                className={`inline-block size-2.5 rounded-full ${
                  mqttConnected ? "bg-green-500" : "bg-red-500"
                }`}
              />
              <span className="text-xs text-muted-foreground">
                MQTT {mqttConnected ? "Connected" : "Disconnected"}
              </span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>
    </Sidebar>
  )
}
