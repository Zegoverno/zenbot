# Insert a hidden test function into the `mod tests {` block of a Rust file (coding-zen-* eval checks).
import sys
path, test = sys.argv[1], open(sys.argv[2]).read()
s = open(path).read()
name = test.split("fn ", 1)[1].split("(", 1)[0]
if f"fn {name}(" in s:
    sys.exit(0)  # already there (a reference solution that brings its own test)
i = s.index("mod tests {")
j = s.index("\n", i) + 1
open(path, "w").write(s[:j] + test + "\n" + s[j:])
