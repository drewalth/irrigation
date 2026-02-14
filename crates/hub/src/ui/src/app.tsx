import { useState } from "preact/hooks"
import DashboardPage from "./app/dashboard/page"

export function App() {
  const [currentPage, setCurrentPage] = useState("overview")
  return <DashboardPage currentPage={currentPage} onNavigate={setCurrentPage} />
}
