import clsx from "clsx";
import { ChevronDown, Loader2, Search } from "lucide-react";
import { forwardRef, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode } from "react";

type Variant = "primary" | "secondary" | "ghost" | "danger" | "glass";
type Size = "sm" | "md" | "lg";

const variants: Record<Variant, string> = {
  primary: "bg-accent text-white hover:bg-accent-strong shadow-[0_8px_24px_-8px_var(--color-accent)]",
  secondary: "bg-white/10 text-fg hover:bg-white/15",
  ghost: "text-dim hover:text-fg hover:bg-white/8",
  danger: "bg-live/15 text-live hover:bg-live/25",
  glass: "bg-black/40 text-white backdrop-blur-xl hover:bg-black/55 ring-1 ring-white/10",
};
const sizes: Record<Size, string> = {
  sm: "h-8 px-3 text-[13px] gap-1.5 rounded-lg",
  md: "h-10 px-4 text-sm gap-2 rounded-xl",
  lg: "h-12 px-6 text-[15px] gap-2.5 rounded-2xl",
};

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  icon?: ReactNode;
  loading?: boolean;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", icon, loading, className, children, disabled, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      disabled={disabled || loading}
      className={clsx(
        "inline-flex items-center justify-center font-semibold whitespace-nowrap select-none",
        "transition-[background,color,transform,box-shadow] duration-150 active:scale-[0.97]",
        "disabled:opacity-40 disabled:pointer-events-none",
        variants[variant],
        sizes[size],
        className,
      )}
      {...rest}
    >
      {loading ? <Loader2 className="size-4 animate-spin" /> : icon}
      {children}
    </button>
  );
});

interface IconButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  label: string;
  active?: boolean;
  size?: "sm" | "md" | "lg";
  variant?: "ghost" | "glass" | "solid";
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { label, active, size = "md", variant = "ghost", className, children, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      aria-label={label}
      title={label}
      className={clsx(
        "inline-flex items-center justify-center rounded-full transition-[background,color,transform] duration-150 active:scale-90",
        size === "sm" && "size-8",
        size === "md" && "size-10",
        size === "lg" && "size-14",
        variant === "ghost" && (active ? "text-accent-strong bg-accent/15" : "text-dim hover:text-fg hover:bg-white/10"),
        variant === "glass" && "text-white bg-black/35 backdrop-blur-xl ring-1 ring-white/10 hover:bg-black/55",
        variant === "solid" && "text-black bg-white hover:bg-white/90",
        className,
      )}
      {...rest}
    >
      {children}
    </button>
  );
});

export function Badge({ children, tone = "default", className }: { children: ReactNode; tone?: "default" | "live" | "accent" | "gold"; className?: string }) {
  return (
    <span
      className={clsx(
        "inline-flex items-center rounded-[5px] px-1.5 py-px text-[10px] font-bold tracking-wide uppercase leading-4",
        tone === "default" && "bg-white/10 text-dim",
        tone === "live" && "bg-live text-white",
        tone === "accent" && "bg-accent/20 text-accent-strong",
        tone === "gold" && "bg-gold/15 text-gold",
        className,
      )}
    >
      {children}
    </span>
  );
}

export function LiveDot({ className }: { className?: string }) {
  return (
    <span className={clsx("relative inline-flex size-2", className)}>
      <span className="absolute inline-flex size-full animate-ping rounded-full bg-live opacity-60" />
      <span className="relative inline-flex size-2 rounded-full bg-live" />
    </span>
  );
}

export function ProgressBar({ value, className, tone = "accent" }: { value: number; className?: string; tone?: "accent" | "white" | "live" }) {
  return (
    <div className={clsx("h-1 overflow-hidden rounded-full bg-white/15", className)}>
      <div
        className={clsx(
          "h-full rounded-full transition-[width] duration-500",
          tone === "accent" && "bg-accent",
          tone === "white" && "bg-white",
          tone === "live" && "bg-live",
        )}
        style={{ width: `${Math.round(Math.min(1, Math.max(0, value)) * 100)}%` }}
      />
    </div>
  );
}

export function Spinner({ className, label }: { className?: string; label?: string }) {
  return (
    <div className={clsx("flex items-center gap-3 text-dim", className)}>
      <Loader2 className="size-5 animate-spin" />
      {label && <span className="text-sm">{label}</span>}
    </div>
  );
}

export function EmptyState({ icon, title, text, action, className }: { icon?: ReactNode; title: string; text?: ReactNode; action?: ReactNode; className?: string }) {
  return (
    <div className={clsx("flex flex-col items-center justify-center gap-3 px-6 py-16 text-center animate-fade-in", className)}>
      {icon && <div className="mb-1 grid size-14 place-items-center rounded-2xl bg-white/5 text-dim [&>svg]:size-7">{icon}</div>}
      <h3 className="text-lg font-semibold">{title}</h3>
      {text && <p className="max-w-md text-sm leading-relaxed text-dim">{text}</p>}
      {action && <div className="mt-2">{action}</div>}
    </div>
  );
}

export const TextField = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement> & { label?: string; hint?: ReactNode; icon?: ReactNode }>(
  function TextField({ label, hint, icon, className, ...rest }, ref) {
    return (
      <label className={clsx("flex flex-col gap-1.5", className)}>
        {label && <span className="text-[13px] font-medium text-dim">{label}</span>}
        <span className="relative flex items-center">
          {icon && <span className="pointer-events-none absolute left-3 text-faint [&>svg]:size-4">{icon}</span>}
          <input
            ref={ref}
            className={clsx(
              "h-11 w-full rounded-xl bg-white/[0.06] px-3.5 text-[15px] text-fg ring-1 ring-white/10 outline-none",
              "placeholder:text-faint transition-[box-shadow,background] focus:bg-white/[0.08] focus:ring-2 focus:ring-accent",
              icon && "pl-9",
            )}
            {...rest}
          />
        </span>
        {hint && <span className="text-xs text-faint">{hint}</span>}
      </label>
    );
  },
);

export function Switch({ checked, onChange, label, description }: { checked: boolean; onChange: (v: boolean) => void; label: string; description?: ReactNode }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className="flex w-full items-center justify-between gap-6 rounded-xl px-1 py-2 text-left"
    >
      <span>
        <span className="block text-[15px] font-medium">{label}</span>
        {description && <span className="mt-0.5 block text-[13px] text-dim">{description}</span>}
      </span>
      <span className={clsx("relative h-6 w-10 shrink-0 rounded-full transition-colors", checked ? "bg-accent" : "bg-white/15")}>
        <span
          className={clsx(
            "absolute top-0.5 size-5 rounded-full bg-white shadow transition-transform duration-200",
            checked ? "translate-x-[18px]" : "translate-x-0.5",
          )}
        />
      </span>
    </button>
  );
}

export function Segmented<T extends string>({ value, options, onChange, className }: { value: T; options: { value: T; label: ReactNode }[]; onChange: (v: T) => void; className?: string }) {
  return (
    <div className={clsx("inline-flex rounded-xl bg-white/[0.06] p-1 ring-1 ring-white/5", className)}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          onClick={() => onChange(o.value)}
          className={clsx(
            "h-8 rounded-lg px-3.5 text-[13px] font-semibold transition-colors",
            o.value === value ? "bg-white/15 text-fg shadow-sm" : "text-dim hover:text-fg",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** Filter field with a search icon (side panels, list headers). */
export function SearchInput({ value, onChange, placeholder, className }: { value: string; onChange: (v: string) => void; placeholder?: string; className?: string }) {
  return (
    <label className={clsx("relative flex items-center", className)}>
      <Search className="pointer-events-none absolute left-3 size-4 text-faint" />
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="h-9 w-full rounded-lg bg-white/[0.06] pl-9 pr-3 text-sm outline-none ring-1 ring-white/[0.06] placeholder:text-faint focus:ring-accent"
      />
    </label>
  );
}

/** A side-panel list entry with an optional icon and count; `dense` for the tighter library panel. */
export function NavRow({
  active,
  onClick,
  count,
  icon,
  inset,
  dense,
  children,
}: {
  active: boolean;
  onClick: () => void;
  count?: number;
  icon?: ReactNode;
  inset?: boolean;
  dense?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      className={clsx(
        "flex w-full items-center gap-2.5 rounded-lg px-3 text-left text-[13.5px] transition-colors",
        dense ? "min-h-8 py-1" : "min-h-9 py-1.5",
        inset && (dense ? "pl-5" : "pl-8"),
        active ? "bg-accent/15 font-semibold text-fg" : "text-dim hover:bg-white/[0.04] hover:text-fg",
      )}
    >
      {icon && <span className={clsx("shrink-0", active ? "text-accent-strong" : "text-faint")}>{icon}</span>}
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {count != null && <span className={clsx("shrink-0 text-[11px] tabular-nums text-faint", !dense && "font-normal")}>{count.toLocaleString()}</span>}
    </button>
  );
}

/** Small-caps section title that folds its section. */
export function FoldHeader({ open, onToggle, count, children }: { open: boolean; onToggle: () => void; count: number; children: ReactNode }) {
  return (
    <button
      onClick={onToggle}
      aria-expanded={open}
      className="flex w-full items-center gap-1.5 px-3 py-1.5 text-[11px] font-bold uppercase tracking-[0.12em] text-faint hover:text-dim"
    >
      <ChevronDown className={clsx("size-3.5 transition-transform", !open && "-rotate-90")} />
      {children}
      <span className="ml-auto font-medium normal-case tracking-normal">{count}</span>
    </button>
  );
}
