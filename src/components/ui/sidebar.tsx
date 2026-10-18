import * as React from "react"
import { PanelLeft, Search } from "lucide-react"
import { cn } from "@/lib/utils"

/* -------------------------------------------------------------------------- */
/* Sidebar Context & Hook                                                     */
/* -------------------------------------------------------------------------- */

interface SidebarContextValue {
  open: boolean
  setOpen: (open: boolean | ((prev: boolean) => boolean)) => void
  toggleSidebar: () => void
  isMobile: boolean
  width: string
  collapsedWidth: string
  activeItem: string | null
  setActiveItem: (id: string | null) => void
}

const SidebarContext = React.createContext<SidebarContextValue | undefined>(undefined)

export function useSidebar() {
  const context = React.useContext(SidebarContext)
  if (!context) {
    throw new Error("useSidebar must be used within a SidebarProvider")
  }
  return context
}

export interface SidebarProviderProps {
  children: React.ReactNode
  defaultOpen?: boolean
  open?: boolean
  onOpenChange?: (open: boolean) => void
  width?: string
  collapsedWidth?: string
  className?: string
  defaultActiveItem?: string | null
}

export function SidebarProvider({
  children,
  defaultOpen = true,
  open: controlledOpen,
  onOpenChange,
  width = "w-60",
  collapsedWidth = "w-14",
  className,
  defaultActiveItem = null,
}: SidebarProviderProps) {
  const [uncontrolledOpen, setUncontrolledOpen] = React.useState(defaultOpen)
  const [activeItem, setActiveItem] = React.useState<string | null>(defaultActiveItem)
  const [isMobile, setIsMobile] = React.useState(false)

  const isControlled = controlledOpen !== undefined
  const open = isControlled ? controlledOpen : uncontrolledOpen

  const setOpen = React.useCallback(
    (value: boolean | ((prev: boolean) => boolean)) => {
      const nextOpen = typeof value === "function" ? value(open) : value
      if (!isControlled) {
        setUncontrolledOpen(nextOpen)
      }
      onOpenChange?.(nextOpen)
    },
    [isControlled, open, onOpenChange]
  )

  const toggleSidebar = React.useCallback(() => {
    setOpen((prev) => !prev)
  }, [setOpen])

  React.useEffect(() => {
    const checkMobile = () => {
      setIsMobile(window.innerWidth < 768)
    }
    checkMobile()
    window.addEventListener("resize", checkMobile)
    return () => window.removeEventListener("resize", checkMobile)
  }, [])

  return (
    <SidebarContext.Provider
      value={{
        open,
        setOpen,
        toggleSidebar,
        isMobile,
        width,
        collapsedWidth,
        activeItem,
        setActiveItem,
      }}
    >
      <div
        className={cn(
          "flex min-h-full w-full select-none font-sans text-[var(--text-main)]",
          className
        )}
      >
        {children}
      </div>
    </SidebarContext.Provider>
  )
}

/* -------------------------------------------------------------------------- */
/* Sidebar Root Container                                                     */
/* -------------------------------------------------------------------------- */

export interface SidebarProps extends React.HTMLAttributes<HTMLDivElement> {
  side?: "left" | "right"
  variant?: "sidebar" | "floating" | "inset"
  collapsible?: boolean
}

export const Sidebar = React.forwardRef<HTMLDivElement, SidebarProps>(
  (
    {
      side = "left",
      variant = "sidebar",
      collapsible = true,
      className,
      children,
      ...props
    },
    ref
  ) => {
    const { open, width, collapsedWidth } = useSidebar()
    const currentWidth = open || !collapsible ? width : collapsedWidth

    return (
      <aside
        ref={ref}
        data-state={open ? "expanded" : "collapsed"}
        data-side={side}
        data-variant={variant}
        className={cn(
          "macos-sidebar group/sidebar relative flex flex-col shrink-0 transition-[width] duration-[var(--kc-duration-dialog)] ease-[var(--kc-ease-out-ui)] z-20 overflow-hidden",
          "border-r border-[var(--border-subtle)]",
          variant === "floating" && "m-2 rounded-[var(--kc-radius-surface)] border shadow-sm",
          variant === "inset" && "bg-transparent border-r-0",
          currentWidth,
          className
        )}
        {...props}
      >
        {children}
      </aside>
    )
  }
)
Sidebar.displayName = "Sidebar"

/* -------------------------------------------------------------------------- */
/* Sidebar Trigger (Collapse / Expand Button)                                 */
/* -------------------------------------------------------------------------- */

export interface SidebarTriggerProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement> {}

export const SidebarTrigger = React.forwardRef<
  HTMLButtonElement,
  SidebarTriggerProps
>(({ className, onClick, ...props }, ref) => {
  const { toggleSidebar, open } = useSidebar()

  return (
    <button
      ref={ref}
      type="button"
      aria-label={open ? "Collapse Sidebar" : "Expand Sidebar"}
      title={open ? "Collapse Sidebar" : "Expand Sidebar"}
      onClick={(e) => {
        onClick?.(e)
        toggleSidebar()
      }}
      className={cn(
        "inline-flex items-center justify-center h-7 w-7 rounded-[var(--kc-radius-control)] text-[var(--text-secondary)] hover:text-[var(--text-main)] hover:bg-black/5 dark:hover:bg-white/10 active:bg-black/10 dark:active:bg-white/15 transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-[var(--kc-color-focus)] shrink-0 cursor-default select-none",
        className
      )}
      {...props}
    >
      <PanelLeft className="h-4 w-4" />
    </button>
  )
})
SidebarTrigger.displayName = "SidebarTrigger"

/* -------------------------------------------------------------------------- */
/* Sidebar Header, Brand & Footer                                             */
/* -------------------------------------------------------------------------- */

export const SidebarHeader = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => {
  const { open } = useSidebar()
  return (
    <div
      ref={ref}
      data-state={open ? "expanded" : "collapsed"}
      className={cn(
        "flex flex-col gap-2 p-2.5 shrink-0 border-b border-[var(--border-subtle)] transition-[padding,gap] duration-[var(--kc-duration-dialog)] ease-[var(--kc-ease-out-ui)]",
        !open && "items-center justify-center px-1 py-2",
        className
      )}
      {...props}
    >
      {children}
    </div>
  )
})
SidebarHeader.displayName = "SidebarHeader"

export const SidebarBrand = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => {
  const { open } = useSidebar()
  if (!open) return null

  return (
    <div
      ref={ref}
      className={cn("flex items-center gap-2 min-w-0 flex-1 overflow-hidden", className)}
      {...props}
    >
      {children}
    </div>
  )
})
SidebarBrand.displayName = "SidebarBrand"

export const SidebarFooter = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => {
  const { open } = useSidebar()
  return (
    <div
      ref={ref}
      data-state={open ? "expanded" : "collapsed"}
      className={cn(
        "flex flex-col gap-2 p-2.5 shrink-0 border-t border-[var(--border-subtle)] mt-auto transition-[padding,gap] duration-[var(--kc-duration-dialog)] ease-[var(--kc-ease-out-ui)]",
        !open && "items-center justify-center px-1 py-2",
        className
      )}
      {...props}
    >
      {children}
    </div>
  )
})
SidebarFooter.displayName = "SidebarFooter"

export const SidebarFooterText = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => {
  const { open } = useSidebar()
  if (!open) return null

  return (
    <div
      ref={ref}
      className={cn("flex flex-col min-w-0 flex-1 overflow-hidden", className)}
      {...props}
    >
      {children}
    </div>
  )
})
SidebarFooterText.displayName = "SidebarFooterText"

/* -------------------------------------------------------------------------- */
/* Sidebar Search / Input                                                     */
/* -------------------------------------------------------------------------- */

export interface SidebarSearchProps
  extends React.InputHTMLAttributes<HTMLInputElement> {
  hideWhenCollapsed?: boolean
}

export const SidebarSearch = React.forwardRef<
  HTMLInputElement,
  SidebarSearchProps
>(({ className, hideWhenCollapsed = false, placeholder = "Search...", ...props }, ref) => {
  const { open, toggleSidebar } = useSidebar()

  if (!open) {
    if (hideWhenCollapsed) return null
    return (
      <button
        type="button"
        onClick={toggleSidebar}
        title="Search"
        aria-label="Search"
        className="flex items-center justify-center h-7 w-7 rounded-md text-[var(--text-tertiary)] hover:text-[var(--text-main)] hover:bg-black/5 dark:hover:bg-white/10 transition-colors mx-auto shrink-0 cursor-default"
      >
        <Search className="h-4 w-4" />
      </button>
    )
  }

  return (
    <div className="relative flex items-center w-full">
      <Search className="absolute left-2.5 h-3.5 w-3.5 text-[var(--text-tertiary)] pointer-events-none" />
      <input
        ref={ref}
        type="search"
        placeholder={placeholder}
        className={cn(
          "w-full h-7 pl-8 pr-2.5 text-xs bg-black/5 dark:bg-white/10 text-[var(--text-main)] placeholder-[var(--text-tertiary)] rounded-[var(--kc-radius-control)] border border-black/5 dark:border-white/5 outline-none transition-[background-color,border-color,box-shadow] duration-[var(--kc-duration-press)] ease-[var(--kc-ease-out-ui)] focus:bg-white dark:focus:bg-[#2A2A2E] focus:border-[var(--kc-color-focus)] focus:ring-1 focus:ring-[var(--kc-color-focus)]/30",
          className
        )}
        {...props}
      />
    </div>
  )
})
SidebarSearch.displayName = "SidebarSearch"

/* -------------------------------------------------------------------------- */
/* Sidebar Content & Scroll Region                                            */
/* -------------------------------------------------------------------------- */

export const SidebarContent = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => {
  const { open } = useSidebar()
  return (
    <div
      ref={ref}
      data-state={open ? "expanded" : "collapsed"}
      className={cn(
        "flex flex-col flex-1 overflow-y-auto overflow-x-hidden p-2 gap-3 min-h-0",
        !open && "px-1.5 gap-2 items-center",
        className
      )}
      {...props}
    >
      {children}
    </div>
  )
})
SidebarContent.displayName = "SidebarContent"

/* -------------------------------------------------------------------------- */
/* Sidebar Grouping Components                                                */
/* -------------------------------------------------------------------------- */

export const SidebarGroup = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => (
  <div ref={ref} className={cn("flex flex-col gap-0.5 w-full", className)} {...props}>
    {children}
  </div>
))
SidebarGroup.displayName = "SidebarGroup"

export interface SidebarGroupLabelProps
  extends React.HTMLAttributes<HTMLDivElement> {
  action?: React.ReactNode
}

export const SidebarGroupLabel = React.forwardRef<
  HTMLDivElement,
  SidebarGroupLabelProps
>(({ className, children, action, ...props }, ref) => {
  const { open } = useSidebar()
  if (!open) return null

  return (
    <div
      ref={ref}
      className={cn(
        "flex items-center justify-between px-2.5 py-1 text-[10px] font-semibold tracking-wider text-[var(--text-tertiary)] uppercase select-none",
        className
      )}
      {...props}
    >
      <span className="truncate">{children}</span>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  )
})
SidebarGroupLabel.displayName = "SidebarGroupLabel"

export const SidebarGroupContent = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => (
  <div ref={ref} className={cn("flex flex-col gap-0.5 w-full", className)} {...props}>
    {children}
  </div>
))
SidebarGroupContent.displayName = "SidebarGroupContent"

/* -------------------------------------------------------------------------- */
/* Sidebar Menu & Items                                                       */
/* -------------------------------------------------------------------------- */

export const SidebarMenu = React.forwardRef<
  HTMLUListElement,
  React.HTMLAttributes<HTMLUListElement>
>(({ className, children, ...props }, ref) => (
  <ul ref={ref} className={cn("flex flex-col gap-0.5 w-full m-0 p-0 list-none", className)} {...props}>
    {children}
  </ul>
))
SidebarMenu.displayName = "SidebarMenu"

export const SidebarMenuItem = React.forwardRef<
  HTMLLIElement,
  React.HTMLAttributes<HTMLLIElement>
>(({ className, children, ...props }, ref) => (
  <li ref={ref} className={cn("relative w-full list-none flex justify-center", className)} {...props}>
    {children}
  </li>
))
SidebarMenuItem.displayName = "SidebarMenuItem"

export interface SidebarMenuButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  active?: boolean
  icon?: React.ReactNode
  badge?: React.ReactNode
  id?: string
  tooltip?: string
}

export const SidebarMenuButton = React.forwardRef<
  HTMLButtonElement,
  SidebarMenuButtonProps
>(
  (
    {
      className,
      active: controlledActive,
      icon,
      badge,
      children,
      id,
      onClick,
      tooltip,
      ...props
    },
    ref
  ) => {
    const { open, activeItem, setActiveItem } = useSidebar()
    const isActive = controlledActive !== undefined ? controlledActive : (id && activeItem === id)

    const handleClick = (e: React.MouseEvent<HTMLButtonElement>) => {
      if (id) {
        setActiveItem(id)
      }
      onClick?.(e)
    }

    const titleText = tooltip || (typeof children === "string" ? children : undefined)

    return (
      <button
        ref={ref}
        type="button"
        title={!open ? titleText : undefined}
        aria-label={typeof children === "string" ? children : undefined}
        onClick={handleClick}
        className={cn(
          "flex items-center text-left text-xs font-medium transition-[background-color,color,box-shadow] duration-[var(--kc-duration-press)] ease-[var(--kc-ease-out-ui)] cursor-default outline-none select-none",
          open
            ? "w-full gap-2.5 px-2.5 py-1.5 rounded-[var(--kc-radius-control)]"
            : "h-8 w-8 justify-center rounded-[var(--kc-radius-control)] px-0 shrink-0 mx-auto",
          isActive
            ? "bg-[var(--kc-color-selection)] text-[var(--kc-color-text-primary)] font-semibold shadow-xs"
            : "text-[var(--text-secondary)] hover:bg-black/5 dark:hover:bg-white/10 hover:text-[var(--text-main)] active:bg-black/10 dark:active:bg-white/15",
          className
        )}
        {...props}
      >
        {icon && (
          <span
            className={cn(
              "shrink-0 h-4 w-4 flex items-center justify-center transition-colors",
              isActive ? "text-[var(--kc-color-text-primary)]" : "text-[var(--text-tertiary)] group-hover:text-[var(--text-main)]"
            )}
          >
            {icon}
          </span>
        )}
        {open && <span className="flex-1 truncate">{children}</span>}
        {open && badge && (
          <span
            className={cn(
              "shrink-0 text-[10px] px-1.5 py-0.5 rounded-full font-semibold",
              isActive
                ? "bg-white/10 text-[var(--kc-color-text-primary)]"
                : "bg-black/5 dark:bg-white/10 text-[var(--text-secondary)]"
            )}
          >
            {badge}
          </span>
        )}
      </button>
    )
  }
)
SidebarMenuButton.displayName = "SidebarMenuButton"

/* -------------------------------------------------------------------------- */
/* Sidebar Submenu (Tree Navigation)                                          */
/* -------------------------------------------------------------------------- */

export const SidebarMenuSub = React.forwardRef<
  HTMLUListElement,
  React.HTMLAttributes<HTMLUListElement>
>(({ className, children, ...props }, ref) => {
  const { open } = useSidebar()
  if (!open) return null

  return (
    <ul
      ref={ref}
      className={cn(
        "flex flex-col gap-0.5 ml-4 pl-2.5 border-l border-black/10 dark:border-white/10 my-0.5 list-none",
        className
      )}
      {...props}
    >
      {children}
    </ul>
  )
})
SidebarMenuSub.displayName = "SidebarMenuSub"

export const SidebarMenuSubItem = React.forwardRef<
  HTMLLIElement,
  React.HTMLAttributes<HTMLLIElement>
>(({ className, children, ...props }, ref) => (
  <li ref={ref} className={cn("relative w-full list-none", className)} {...props}>
    {children}
  </li>
))
SidebarMenuSubItem.displayName = "SidebarMenuSubItem"

export interface SidebarMenuSubButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  active?: boolean
  icon?: React.ReactNode
}

export const SidebarMenuSubButton = React.forwardRef<
  HTMLButtonElement,
  SidebarMenuSubButtonProps
>(({ className, active, icon, children, ...props }, ref) => (
  <button
    ref={ref}
    type="button"
    className={cn(
      "w-full flex items-center gap-2 px-2 py-1 rounded-[var(--kc-radius-control)] text-left text-xs transition-colors outline-none select-none",
      active
        ? "text-[var(--kc-color-text-primary)] font-semibold bg-[var(--kc-color-selection)]"
        : "text-[var(--text-secondary)] hover:text-[var(--text-main)] hover:bg-black/5 dark:hover:bg-white/5",
      className
    )}
    {...props}
  >
    {icon && <span className="shrink-0 h-3.5 w-3.5">{icon}</span>}
    <span className="truncate">{children}</span>
  </button>
))
SidebarMenuSubButton.displayName = "SidebarMenuSubButton"

/* -------------------------------------------------------------------------- */
/* Sidebar Separator                                                          */
/* -------------------------------------------------------------------------- */

export const SidebarSeparator = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => {
  const { open } = useSidebar()
  return (
    <div
      ref={ref}
      className={cn(
        "h-[1px] bg-[var(--border-subtle)] my-1 transition-[width] duration-[var(--kc-duration-dialog)] ease-[var(--kc-ease-out-ui)]",
        open ? "w-full" : "w-6 mx-auto",
        className
      )}
      {...props}
    />
  )
})
SidebarSeparator.displayName = "SidebarSeparator"
