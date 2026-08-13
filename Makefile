TSGO_REPO = https://github.com/microsoft/typescript-go.git
TSGO_REV ?= $(shell cat artifacts/tsgo.rev)

tsgo:
	rm -rf .tsgo-build
	git init -q .tsgo-build
	git -C .tsgo-build remote add origin $(TSGO_REPO)
	git -C .tsgo-build fetch -q --depth 1 origin $(TSGO_REV)
	git -C .tsgo-build checkout -q FETCH_HEAD
	cd .tsgo-build && GOOS=wasip1 GOARCH=wasm go build -o ../artifacts/tsgo.wasm ./cmd/tsgo
	zstd -19 -T0 -q -f --rm -o artifacts/tsgo.wasm.zst artifacts/tsgo.wasm
	git -C .tsgo-build rev-parse HEAD | tr -d '\n' > artifacts/tsgo.rev
	shasum -a 256 artifacts/tsgo.wasm.zst | cut -d' ' -f1 | tr -d '\n' > artifacts/tsgo.sha256
	rm -rf .tsgo-build

test:
	cargo test --release
