import { useState, forwardRef } from "react";
import { Eye, EyeOff } from "lucide-react";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

type PasswordInputProps = React.InputHTMLAttributes<HTMLInputElement> & {
  /** Initial revealed state. The component is uncontrolled w.r.t. the
   *  toggle — the caller still controls `value` / `onChange`. */
  defaultRevealed?: boolean;
  /** Wrapping div className, since the eye sits next to the input. */
  wrapperClassName?: string;
};

/** `<Input>` with a trailing eye/eye-off button that flips
 *  `type="password"` ↔ `type="text"`. The actual value stays in the
 *  caller's state; we just toggle masking. Reused for the bearer
 *  token (gap #74), variable values (gap #73), and the global
 *  authorization header (gap #75). */
export const PasswordInput = forwardRef<HTMLInputElement, PasswordInputProps>(
  function PasswordInput(
    { className, wrapperClassName, defaultRevealed = false, type: _type, ...rest },
    ref,
  ) {
    const [revealed, setRevealed] = useState(defaultRevealed);
    return (
      <div className={cn("relative flex items-center", wrapperClassName)}>
        <Input
          ref={ref}
          type={revealed ? "text" : "password"}
          className={cn("pr-9", className)}
          {...rest}
        />
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          onClick={() => setRevealed((r) => !r)}
          tabIndex={-1}
          title={revealed ? "Hide" : "Reveal"}
          aria-label={revealed ? "Hide value" : "Reveal value"}
          className="absolute right-1 top-1/2 -translate-y-1/2 size-7 text-muted-foreground hover:text-foreground"
        >
          {revealed ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
        </Button>
      </div>
    );
  },
);
