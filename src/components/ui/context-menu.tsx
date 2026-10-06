import * as React from "react"
import { createPortal } from "react-dom"
import { cn } from "@/lib/utils"

export interface ContextMenuItem {
  id: string
  label: string
  icon?: React.ReactNode
  shortcut?: string
  destructive?: boolean
  disabled?: boolean
  onClick?: () => void
}

export interface ContextMenuProps {
  trigger: React.ReactNode
  items: (ContextMenuItem | "separator")[]
  className?: string
}

const ContextMenu: React.FC<ContextMenuProps> = ({
  trigger,
  items,
  className,
}) => {
  const [isOpen, setIsOpen] = React.useState(false)
  const [coords, setCoords] = React.useState({ x: 0, y: 0 })
  const mounted = typeof document !== "undefined"
  const menuRef = React.useRef<HTMLDivElement>(null)
  const triggerRef = React.useRef<HTMLDivElement>(null)


  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault()
    e.stopPropagation()
    setCoords({ x: e.clientX, y: e.clientY })
    setIsOpen(true)
  }

  React.useLayoutEffect(() => {
    if (!isOpen || !menuRef.current) return
    const el = menuRef.current
    const rect = el.getBoundingClientRect()
    const pad = 8
    let x = coords.x
    let y = coords.y
    if (x + rect.width > window.innerWidth - pad) {
      x = Math.max(pad, window.innerWidth - rect.width - pad)
    }
    if (y + rect.height > window.innerHeight - pad) {
      y = Math.max(pad, window.innerHeight - rect.height - pad)
    }
    if (x !== coords.x || y !== coords.y) {
      setCoords({ x, y })
    }
  }, [isOpen, coords.x, coords.y])

  React.useEffect(() => {
    if (!isOpen) return

    const handlePointerDown = (e: PointerEvent) => {
      const target = e.target as Node
      if (menuRef.current?.contains(target)) return
      if (triggerRef.current?.contains(target) && e.button === 2) return
      setIsOpen(false)
    }

    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setIsOpen(false)
    }

    const id = window.setTimeout(() => {
      document.addEventListener("pointerdown", handlePointerDown)
    }, 0)
    window.addEventListener("keydown", handleKeyDown)

    return () => {
      window.clearTimeout(id)
      document.removeEventListener("pointerdown", handlePointerDown)
      window.removeEventListener("keydown", handleKeyDown)
    }
  }, [isOpen])

  return (
    <>
      <div
        ref={triggerRef}
        onContextMenu={handleContextMenu}
        className="inline-block"
      >
        {trigger}
      </div>
      {mounted &&
        isOpen &&
        createPortal(
          <div
            ref={menuRef}
            role="menu"
            onContextMenu={(e) => e.preventDefault()}
            className={cn(
              "fixed z-50 min-w-[200px] rounded-xl macos-popover p-1.5 border border-[var(--border-color)] shadow-xl animate-in fade-in-50 zoom-in-95 duration-100 select-none",
              className
            )}
            style={{ left: coords.x, top: coords.y }}
          >
            {items.map((item, idx) => {
              if (item === "separator") {
                return <div key={`sep-${idx}`} className="my-1.5 h-px bg-[var(--border-subtle)]" />
              }

              return (
                <button
                  key={item.id}
                  type="button"
                  role="menuitem"
                  disabled={item.disabled}
                  onClick={() => {
                    if (!item.disabled) {
                      item.onClick?.()
                      setIsOpen(false)
                    }
                  }}
                  className={cn(
                    "group flex h-7 w-full items-center justify-between rounded-lg px-2.5 text-xs transition-colors cursor-default",
                    item.destructive
                      ? "text-[#FF3B30] hover:bg-[#FF3B30] hover:text-white"
                      : "text-[var(--text-main)] hover:bg-[#007AFF] hover:text-white",
                    item.disabled && "opacity-50 cursor-not-allowed hover:bg-transparent hover:text-[var(--text-main)]"
                  )}
                >
                  <div className="flex items-center gap-2.5 truncate">
                    {item.icon && (
                      <span className="flex items-center justify-center shrink-0 w-4 h-4 [&>svg]:w-3.5 [&>svg]:h-3.5 [&>svg]:shrink-0 text-[var(--text-secondary)] group-hover:text-white transition-colors">
                        {item.icon}
                      </span>
                    )}
                    <span className="truncate font-medium">{item.label}</span>
                  </div>
                  {item.shortcut && (
                    <span className="ml-4 text-[10px] tracking-wider text-[var(--text-tertiary)] group-hover:text-white/80 font-mono">
                      {item.shortcut}
                    </span>
                  )}
                </button>
              )
            })}
          </div>,
          document.body
        )}
    </>
  )
}

export { ContextMenu }
