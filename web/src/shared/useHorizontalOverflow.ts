import { useEffect, useRef, useState } from "react";

export function useHorizontalOverflow<T extends HTMLElement>() {
  const elementRef = useRef<T>(null);
  const [overflows, setOverflows] = useState(false);

  useEffect(() => {
    const element = elementRef.current;
    if (element === null) {
      return;
    }
    const update = () => {
      setOverflows(element.scrollWidth > element.clientWidth + 1);
    };
    update();

    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", update);
      return () => {
        window.removeEventListener("resize", update);
      };
    }

    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  }, []);

  return { elementRef, overflows } as const;
}
