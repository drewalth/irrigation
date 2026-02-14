import { useState } from "preact/hooks";
import DashboardPage from "./app/dashboard/page";
import { Toaster } from "@/components/ui/sonner";

export function App() {
  const [currentPage, setCurrentPage] = useState("overview");
  return (
    <>
      <DashboardPage currentPage={currentPage} onNavigate={setCurrentPage} />
      <Toaster />
    </>
  );
}
