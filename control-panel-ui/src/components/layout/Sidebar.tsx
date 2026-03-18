'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'
import {
  Activity, BarChart2, Cpu, Eye, FlaskConical,
  LayoutDashboard, ShieldAlert, Zap,
} from 'lucide-react'
import clsx from 'clsx'

const NAV = [
  { href: '/overview',             label: 'System Overview',    icon: LayoutDashboard },
  { href: '/market-intelligence',  label: 'Market Intelligence',icon: Eye },
  { href: '/signals',              label: 'Signal Monitor',     icon: Activity },
  { href: '/portfolio',            label: 'Portfolio',          icon: BarChart2 },
  { href: '/strategy-research',    label: 'Strategy Research',  icon: FlaskConical },
  { href: '/risk-control',         label: 'Risk Control',       icon: ShieldAlert },
  { href: '/execution',            label: 'Execution Monitor',  icon: Zap },
]

export function Sidebar() {
  const pathname = usePathname()

  return (
    <aside className="fixed left-0 top-0 h-full w-56 flex flex-col border-r border-border bg-bg-surface z-40">
      {/* Logo */}
      <div className="flex items-center gap-2 px-4 py-4 border-b border-border">
        <Cpu size={18} className="text-accent-blue" />
        <div>
          <div className="text-text-primary font-semibold text-sm tracking-wide">MISSION</div>
          <div className="text-accent-blue text-xs tracking-widest">CONTROL</div>
        </div>
      </div>

      {/* Navigation */}
      <nav className="flex-1 py-3 overflow-y-auto">
        {NAV.map(({ href, label, icon: Icon }) => {
          const active = pathname === href || pathname.startsWith(href + '/')
          return (
            <Link
              key={href}
              href={href}
              className={clsx(
                'flex items-center gap-3 px-4 py-2.5 mx-2 rounded transition-colors text-xs',
                active
                  ? 'bg-bg-overlay text-text-primary border border-border-bright'
                  : 'text-text-secondary hover:text-text-primary hover:bg-bg-raised',
              )}
            >
              <Icon size={14} className={active ? 'text-accent-blue' : ''} />
              {label}
            </Link>
          )
        })}
      </nav>

      {/* Footer */}
      <div className="px-4 py-3 border-t border-border">
        <div className="text-text-muted text-xs">v0.1.0 — prediction-agent</div>
      </div>
    </aside>
  )
}
