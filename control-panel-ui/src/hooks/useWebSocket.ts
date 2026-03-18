'use client'

// src/hooks/useWebSocket.ts
//
// Reconnecting WebSocket hook.  Dispatches typed WsEvent callbacks.

import { useEffect, useRef, useCallback } from 'react'
import type { WsEvent } from '@/lib/types'

type Handler = (event: WsEvent) => void

const RECONNECT_DELAY_MS = 2_000
const MAX_RECONNECT_DELAY = 30_000

export function useWebSocket(onEvent: Handler) {
  const wsRef          = useRef<WebSocket | null>(null)
  const handlerRef     = useRef<Handler>(onEvent)
  const reconnectDelay = useRef(RECONNECT_DELAY_MS)
  const timerRef       = useRef<ReturnType<typeof setTimeout> | null>(null)
  const unmounted      = useRef(false)

  // Keep handler ref current without re-connecting
  useEffect(() => { handlerRef.current = onEvent }, [onEvent])

  const connect = useCallback(() => {
    if (unmounted.current) return

    // Use NEXT_PUBLIC_WS_URL if set (required in dev because Next.js rewrites
    // do not proxy WebSocket upgrades). Falls back to same-host path for production.
    const url = process.env.NEXT_PUBLIC_WS_URL ?? (() => {
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
      return `${protocol}//${window.location.host}/ws/stream`
    })()
    const ws = new WebSocket(url)
    wsRef.current  = ws

    ws.onopen = () => {
      reconnectDelay.current = RECONNECT_DELAY_MS
    }

    ws.onmessage = (msg) => {
      try {
        const ev: WsEvent = JSON.parse(msg.data as string)
        handlerRef.current(ev)
      } catch {
        // malformed message — skip
      }
    }

    ws.onclose = () => {
      if (unmounted.current) return
      timerRef.current = setTimeout(() => {
        reconnectDelay.current = Math.min(reconnectDelay.current * 2, MAX_RECONNECT_DELAY)
        connect()
      }, reconnectDelay.current)
    }

    ws.onerror = () => ws.close()
  }, [])

  useEffect(() => {
    unmounted.current = false
    connect()
    return () => {
      unmounted.current = true
      if (timerRef.current) clearTimeout(timerRef.current)
      wsRef.current?.close()
    }
  }, [connect])
}
