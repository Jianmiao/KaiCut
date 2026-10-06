import * as React from "react"
import { cn } from "@/lib/utils"

export interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {}

const Input = React.forwardRef<HTMLInputElement, InputProps>(
  ({ className, type, ...props }, ref) => {
    return (
      <input
        type={type}
        className={cn(
          "flex h-8 w-full rounded-md macos-input px-2.5 py-1 text-sm text-[var(--text-main)] placeholder:text-[var(--text-tertiary)] disabled:cursor-not-allowed disabled:opacity-50 transition-colors duration-[var(--kc-duration-press)]",
          className
        )}
        ref={ref}
        {...props}
      />
    )
  }
)
Input.displayName = "Input"

export { Input }
