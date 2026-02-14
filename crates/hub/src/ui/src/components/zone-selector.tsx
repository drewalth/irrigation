import { useZones } from "@/hooks/use-api";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

interface ZoneSelectorProps {
  value: string;
  onChange: (value: string) => void;
}

export function ZoneSelector({ value, onChange }: ZoneSelectorProps) {
  const { data: zones } = useZones();

  return (
    <Select value={value} onValueChange={onChange}>
      <SelectTrigger className="w-[180px]" size="sm">
        <SelectValue placeholder="All zones" />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="__all__">All zones</SelectItem>
        {(zones ?? []).map((z) => (
          <SelectItem key={z.zone_id} value={z.zone_id}>
            {z.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
