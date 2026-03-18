/** @type {import('next').NextConfig} */
const nextConfig = {
  async rewrites() {
    return [
      // Proxy API calls to the Rust backend during development.
      // In production, use a reverse proxy (nginx / Caddy).
      {
        source:      '/api/:path*',
        destination: 'http://localhost:3001/api/:path*',
      },
      {
        source:      '/ws/:path*',
        destination: 'http://localhost:3001/ws/:path*',
      },
    ]
  },
}

export default nextConfig
