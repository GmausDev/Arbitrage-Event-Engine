'use client'

// src/hooks/useApi.ts — SWR-based data fetching hooks.

import useSWR from 'swr'
import {
  fetchHealth,
  fetchPortfolio,
  fetchEquityCurve,
  fetchSignals,
  fetchShocks,
  fetchMarkets,
  fetchStrategies,
  fetchExecutions,
  fetchConfig,
} from '@/lib/api'

// Default refresh interval in milliseconds.
const REFRESH_FAST   = 2_000   // live metrics
const REFRESH_MEDIUM = 5_000   // portfolio, signals
const REFRESH_SLOW   = 10_000  // strategies, config

export const useHealth     = () => useSWR('health',     fetchHealth,      { refreshInterval: REFRESH_FAST })
export const usePortfolio  = () => useSWR('portfolio',  fetchPortfolio,   { refreshInterval: REFRESH_MEDIUM })
export const useEquityCurve= () => useSWR('equity',     fetchEquityCurve, { refreshInterval: REFRESH_MEDIUM })
export const useSignals    = (limit = 100) =>
  useSWR(['signals', limit], () => fetchSignals(limit), { refreshInterval: REFRESH_MEDIUM })
export const useShocks     = (limit = 50) =>
  useSWR(['shocks', limit], () => fetchShocks(limit),  { refreshInterval: REFRESH_MEDIUM })
export const useMarkets    = () => useSWR('markets',    fetchMarkets,     { refreshInterval: REFRESH_FAST })
export const useStrategies = () => useSWR('strategies', fetchStrategies,  { refreshInterval: REFRESH_SLOW })
export const useExecutions = () => useSWR('executions', fetchExecutions,  { refreshInterval: REFRESH_MEDIUM })
export const useConfig     = () => useSWR('config',     fetchConfig,      { refreshInterval: REFRESH_SLOW })
