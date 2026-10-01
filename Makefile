.PHONY: docs docs-clean docs-open

# Build the complete static documentation into build/docs/.
docs:
	bash scripts/build-docs.sh

# Remove generated documentation.
docs-clean:
	rm -rf build/docs

# Serve the built manual locally (requires `make docs` first).
# Uses only the Python standard library; no npm or extra tooling.
docs-open: docs
	python3 -m http.server --directory build/docs 8000
