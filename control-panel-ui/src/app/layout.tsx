import type { Metadata } from 'next'
import './globals.css'
import { Sidebar } from '@/components/layout/Sidebar'
import { TopBar }  from '@/components/layout/TopBar'

export const metadata: Metadata = {
  title:       'Mission Control — Prediction Market Agent',
  description: 'Real-time trading system dashboard',
}

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body className="bg-bg-base text-text-primary font-mono">
        <Sidebar />
        <TopBar />
        {/* Main content — offset for sidebar (w-56) and topbar (h-10) */}
        <main className="ml-56 mt-10 min-h-[calc(100vh-2.5rem)] p-5">
          {children}
        </main>
      </body>
    </html>
  )
}
