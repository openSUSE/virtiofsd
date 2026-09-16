# Container development

From the repository root, use Docker (or set `CONTAINER_ENGINE=podman`) to run the build and tests with the Alpine image and static libraries used by `.gitlab-ci.yml`.
The host needs only the container engine; Rust and development packages stay inside the image.

```bash
./tools/dev-container.sh
./tools/dev-container.sh cargo test --locked --target x86_64-unknown-linux-musl mount_fd::tests
./tools/dev-container.sh cargo fmt -- --check
./tools/dev-container.sh cargo clippy --locked -- -Dwarnings -Aclippy::manual-c-str-literals
./tools/dev-container.sh cargo build --locked --release --target x86_64-unknown-linux-musl
```

The default command runs the complete test suite on x86_64, matching CI.
The container runs as the invoking user's UID/GID so generated files remain user-owned.
Downloads and build artifacts remain under `target/container-cargo` and `target/container-build` in this checkout.
The release binary is `target/container-build/x86_64-unknown-linux-musl/release/virtiofsd`.
Set `VIRTIOFSD_DEV_IMAGE` to change the local image name.
Run Git on the host when using a linked worktree, since its shared Git directory is outside the container's checkout mount.

The `mount_fd::tests` regression tests need neither root nor a guest VM.
They exercise mountinfo decoding, chroot prefixes, and opening a real directory through `MountFds::get()`.
For guest-level validation, repeat the [Linux bind-mount reproduction](https://github.com/virtio-win/kvm-guest-drivers-windows/issues/1431#issuecomment-5688511826) with the newly built daemon and file handles enabled.
Use `--inode-file-handles=mandatory` on a filesystem that supports file handles to rule out a successful run caused by fallback to file descriptors.
Compare spaced and unspaced submount names attached after the guest starts.
