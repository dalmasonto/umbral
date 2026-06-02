import * as React from "react"

// 480px matches Tailwind's `sm` breakpoint. The default shadcn value of 768
// turns the sidebar into a Sheet on common laptop widths with a docked window,
// which made the click-to-collapse trigger appear to "disappear" the sidebar
// entirely. Using a tighter breakpoint keeps the inline icon-rail behavior on
// anything that looks like a desktop browser, while still pushing phones to
// the sheet overlay.
const MOBILE_BREAKPOINT = 480

export function useIsMobile() {
  const [isMobile, setIsMobile] = React.useState<boolean | undefined>(undefined)

  React.useEffect(() => {
    const mql = window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT - 1}px)`)
    const onChange = () => {
      setIsMobile(window.innerWidth < MOBILE_BREAKPOINT)
    }
    mql.addEventListener("change", onChange)
    setIsMobile(window.innerWidth < MOBILE_BREAKPOINT)
    return () => mql.removeEventListener("change", onChange)
  }, [])

  return !!isMobile
}
