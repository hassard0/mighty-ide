# sample.py — multi-language highlighting demo (Python)
import math
from dataclasses import dataclass


@dataclass
class Point:
    """A point in 2D space."""
    x: float
    y: float

    def distance(self, other: "Point") -> float:
        dx = self.x - other.x
        dy = self.y - other.y
        return math.sqrt(dx * dx + dy * dy)


def main() -> None:
    origin = Point(0.0, 0.0)
    p = Point(3.0, 4.0)
    scores = {"alice": 42, "bob": 17}

    d = origin.distance(p)
    print(f"distance = {d}, scores = {scores}")

    for i in range(3):
        if i % 2 == 0:
            print("even:", i)
        else:
            print("odd: ", i)


if __name__ == "__main__":
    main()
