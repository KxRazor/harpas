import java.io.IOException;
import java.io.PrintWriter;
import java.io.UncheckedIOException;
import java.nio.file.Files;
import java.nio.file.Path;

/** A formatted probe output that reports errors even if they first appear while closing. */
final class ProbeOutput extends PrintWriter {
    private final String file;

    ProbeOutput(String file) throws IOException {
        super(Files.newBufferedWriter(Path.of(file)));
        this.file = file;
    }

    @Override public void close() {
        print("# end of probe\n");
        super.close();
        if (checkError()) throw new UncheckedIOException(new IOException("Failed to write " + file));
    }
}
