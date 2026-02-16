import { useState } from "preact/hooks";
import DashboardPage from "./app/dashboard/page";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip"

export function App() {
  const [currentPage, setCurrentPage] = useState("overview");
  return (
    <TooltipProvider>
      <DashboardPage currentPage={currentPage} onNavigate={setCurrentPage} />
      <Toaster />
    </TooltipProvider>
  );
}
