import { toast } from "sonner";

export function useToast() {
  return {
    error: (message: string) =>
      toast.error(message, { position: "bottom-right" }),
    success: (message: string) =>
      toast.success(message, { position: "bottom-right" }),
    info: (message: string) =>
      toast.info(message, { position: "bottom-right" }),
    warning: (message: string) =>
      toast.warning(message, { position: "bottom-right" }),
    loading: (message: string) =>
      toast.loading(message, { position: "bottom-right" }),
  };
}
