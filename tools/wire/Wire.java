import io.netty.buffer.*;
import java.io.*;
import java.net.Socket;
import java.util.*;
import java.util.concurrent.*;
import java.util.zip.*;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.network.*;
import net.minecraft.network.protocol.*;
import net.minecraft.network.protocol.common.*;
import net.minecraft.network.protocol.configuration.*;
import net.minecraft.network.protocol.game.*;
import net.minecraft.network.protocol.handshake.*;
import net.minecraft.network.protocol.login.*;
import net.minecraft.resources.*;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.*;
import net.minecraft.server.packs.resources.*;
import net.minecraft.tags.TagNetworkSerialization;
import net.minecraft.world.InteractionHand;
import net.minecraft.world.level.chunk.*;

/**
 * A minimal client built on the game's own codecs: every packet the server
 * sends is decoded with the release's real STREAM_CODEC and must be consumed
 * exactly; registries are built the way the client builds them; chunk
 * sections are parsed with LevelChunkSection.read.
 *
 * Usage: Wire <port> <protocol> <name> <known|empty|watch>; see wire.sh.
 */
public class Wire {
  static DataInputStream in;
  static OutputStream out;
  static int threshold = -1;
  static int problems = 0;
  static Map<String, Integer> seen = new TreeMap<>();

  static int readVarInt(DataInputStream d) throws IOException {
    int v = 0, i = 0;
    while (true) {
      int b = d.readUnsignedByte();
      v |= (b & 0x7f) << (7 * i++);
      if ((b & 0x80) == 0) return v;
    }
  }

  static byte[] readFrame() throws IOException {
    int len = readVarInt(in);
    byte[] f = new byte[len];
    in.readFully(f);
    if (threshold < 0) return f;
    DataInputStream d = new DataInputStream(new ByteArrayInputStream(f));
    int raw = readVarInt(d);
    byte[] rest = d.readAllBytes();
    if (raw == 0) return rest;
    Inflater inf = new Inflater();
    inf.setInput(rest);
    byte[] o = new byte[raw];
    try {
      int n = 0;
      while (n < raw) n += inf.inflate(o, n, raw - n);
    } catch (DataFormatException e) {
      throw new IOException(e);
    }
    return o;
  }

  static void writeVarInt(ByteArrayOutputStream o, int v) {
    while ((v & ~0x7f) != 0) {
      o.write((v & 0x7f) | 0x80);
      v >>>= 7;
    }
    o.write(v);
  }

  static <T extends PacketListener> void send(ProtocolInfo<T> info, Packet<? super T> p) throws IOException {
    ByteBuf b = Unpooled.buffer();
    info.codec().encode(b, p);
    byte[] body = new byte[b.readableBytes()];
    b.readBytes(body);
    ByteArrayOutputStream frame = new ByteArrayOutputStream();
    if (threshold >= 0) {
      ByteArrayOutputStream inner = new ByteArrayOutputStream();
      if (body.length >= threshold) {
        writeVarInt(inner, body.length);
        DeflaterOutputStream z = new DeflaterOutputStream(inner);
        z.write(body);
        z.finish();
      } else {
        writeVarInt(inner, 0);
        inner.write(body);
      }
      body = inner.toByteArray();
    }
    writeVarInt(frame, body.length);
    frame.write(body);
    out.write(frame.toByteArray());
    out.flush();
  }

  static <T extends PacketListener> Packet<? super T> decode(ProtocolInfo<T> info, byte[] data, String phase) {
    ByteBuf b = Unpooled.wrappedBuffer(data);
    int id = data.length > 0 ? data[0] & 0xff : -1;
    try {
      Packet<? super T> p = info.codec().decode(b);
      String name = phase + "/" + p.type().id().getPath();
      seen.merge(name, 1, Integer::sum);
      if (b.readableBytes() > 0) {
        problems++;
        System.out.println("LEFTOVER " + name + ": " + b.readableBytes() + " bytes unread of " + data.length);
      }
      return p;
    } catch (Exception e) {
      problems++;
      System.out.println("DECODE FAILED " + phase + " id=0x" + Integer.toHexString(id) + " (" + data.length + " bytes): " + e);
      Throwable c = e.getCause();
      while (c != null) {
        System.out.println("   cause: " + c);
        c = c.getCause();
      }
      return null;
    }
  }

  public static void main(String[] a) throws Exception {
    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();
    int port = Integer.parseInt(a[0]);
    int protocol = Integer.parseInt(a[1]);
    String name = a[2];
    boolean watch = a[3].equals("watch");
    boolean known = !a[3].equals("empty");

    Socket s = new Socket("127.0.0.1", port);
    s.setSoTimeout(20000);
    in = new DataInputStream(new BufferedInputStream(s.getInputStream()));
    out = s.getOutputStream();

    send(HandshakeProtocols.SERVERBOUND, new ClientIntentionPacket(protocol, "127.0.0.1", port, ClientIntent.LOGIN));
    send(LoginProtocols.SERVERBOUND, new ServerboundHelloPacket(name, UUID.randomUUID()));
    while (true) {
      Packet<?> p = decode(LoginProtocols.CLIENTBOUND, readFrame(), "login");
      if (p instanceof ClientboundLoginCompressionPacket c) threshold = c.getCompressionThreshold();
      if (p instanceof ClientboundLoginFinishedPacket f) {
        System.out.println("login finished: " + f.gameProfile().name());
        break;
      }
      if (p == null) return;
    }
    send(LoginProtocols.SERVERBOUND, ServerboundLoginAcknowledgedPacket.INSTANCE);

    // Configuration.
    Map<ResourceKey<? extends Registry<?>>, List<RegistrySynchronization.PackedRegistryEntry>> entries = new LinkedHashMap<>();
    Map<ResourceKey<? extends Registry<?>>, TagNetworkSerialization.NetworkPayload> tags = new HashMap<>();
    while (true) {
      Packet<?> p = decode(ConfigurationProtocols.CLIENTBOUND, readFrame(), "config");
      if (p instanceof ClientboundSelectKnownPacks k) {
        System.out.println("offered packs: " + k.knownPacks());
        List<KnownPack> reply = known ? List.of(KnownPack.vanilla(SharedConstants.getCurrentVersion().id())) : List.of();
        send(ConfigurationProtocols.SERVERBOUND, new ServerboundSelectKnownPacks(reply));
      } else if (p instanceof ClientboundRegistryDataPacket r) {
        entries.put(r.registry(), r.entries());
      } else if (p instanceof ClientboundUpdateTagsPacket t) {
        tags.putAll(t.getTags());
      } else if (p instanceof ClientboundFinishConfigurationPacket) {
        break;
      } else if (p == null) {
        return;
      }
    }

    // Static registries take their tags from the packet first, as the
    // client's do: enchantments name item tags.
    for (var e : tags.entrySet()) {
      Registry<?> r = BuiltInRegistries.REGISTRY.getValue(e.getKey().identifier());
      if (r != null) applyTags(r, e.getValue());
    }
    // Build the registries the way the client does: network entries, the
    // rest from the vanilla pack it has.
    Map<ResourceKey<? extends Registry<?>>, RegistryDataLoader.NetworkedRegistryData> netData = new HashMap<>();
    for (var e : entries.entrySet()) {
      netData.put(e.getKey(), new RegistryDataLoader.NetworkedRegistryData(e.getValue(),
          tags.getOrDefault(e.getKey(), TagNetworkSerialization.NetworkPayload.EMPTY)));
    }
    ResourceProvider vanilla = known
        ? new MultiPackResourceManager(PackType.SERVER_DATA, List.of(ServerPacksSource.createVanillaPackSource()))
        : ResourceProvider.EMPTY;
    List<HolderLookup.RegistryLookup<?>> context = RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY)
        .listRegistries().toList();
    RegistryAccess.Frozen synced;
    try {
      synced = RegistryDataLoader.load(netData, vanilla, context, RegistryDataLoader.SYNCHRONIZED_REGISTRIES, Runnable::run).get();
    } catch (ExecutionException e) {
      System.out.println("REGISTRIES REJECTED: " + e.getCause());
      problems++;
      return;
    }
    List<Registry<?>> all = new ArrayList<>();
    RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY).registries().forEach(e -> all.add(e.value()));
    synced.registries().forEach(e -> all.add(e.value()));
    RegistryAccess.Frozen access = new RegistryAccess.ImmutableRegistryAccess(all).freeze();
    // Item stacks need their default components bound, as on load.
    BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(access).forEach(pc -> pc.apply());
    var dims = access.lookupOrThrow(net.minecraft.core.registries.Registries.DIMENSION_TYPE);
    var ow = dims.getOrThrow(net.minecraft.world.level.dimension.BuiltinDimensionTypes.OVERWORLD).value();
    System.out.println("registries built; overworld min_y=" + ow.minY() + " height=" + ow.height());
    int sections = ow.height() / 16;
    send(ConfigurationProtocols.SERVERBOUND, ServerboundFinishConfigurationPacket.INSTANCE);

    ProtocolInfo<ClientGamePacketListener> cb = GameProtocols.CLIENTBOUND_TEMPLATE.bind(RegistryFriendlyByteBuf.decorator(access));
    ProtocolInfo<ServerGamePacketListener> sb = GameProtocols.SERVERBOUND_TEMPLATE.bind(RegistryFriendlyByteBuf.decorator(access),
        new GameProtocols.Context() {
          public boolean hasInfiniteMaterials() { return true; }
          public boolean canUseCommandBlocks() { return false; }
        });
    PalettedContainerFactory pcf = PalettedContainerFactory.create(access);
    int zombie = -1, sheep = -1, chunks = 0, damaged = 0;
    double px = 0, py = 0, pz = 0;
    long start = System.currentTimeMillis();
    int step = 0;
    var zombieType = BuiltInRegistries.ENTITY_TYPE.getValue(Identifier.withDefaultNamespace("zombie"));
    var sheepType = BuiltInRegistries.ENTITY_TYPE.getValue(Identifier.withDefaultNamespace("sheep"));
    int tableSlot = -1;
    boolean dead = false;
    while (System.currentTimeMillis() - start < 26000) {
      Packet<?> p;
      try {
        p = decode(cb, readFrame(), "play");
      } catch (java.net.SocketTimeoutException e) {
        break;
      }
      if (p instanceof ClientboundKeepAlivePacket k) send(sb, new ServerboundKeepAlivePacket(k.getId()));
      if (p instanceof ClientboundPlayerPositionPacket pp) {
        px = pp.change().position().x;
        py = pp.change().position().y;
        pz = pp.change().position().z;
        send(sb, new ServerboundAcceptTeleportationPacket(pp.id()));
      }
      if (p instanceof ClientboundLevelChunkWithLightPacket c) {
        chunks++;
        FriendlyByteBuf rb = c.getChunkData().getReadBuffer();
        try {
          for (int i = 0; i < sections; i++) {
            LevelChunkSection sec = new LevelChunkSection(pcf);
            sec.read(rb);
            boolean saidFluid = sec.hasFluid();
            int saidNonAir = nonEmpty(sec);
            sec.recalcBlockCounts();
            if (saidFluid != sec.hasFluid() || saidNonAir != nonEmpty(sec)) {
              problems++;
              System.out.println("COUNTS WRONG chunk " + c.getX() + "," + c.getZ() + " section " + i
                  + ": sent nonAir=" + saidNonAir + " fluid=" + saidFluid + ", actual nonAir=" + nonEmpty(sec) + " fluid=" + sec.hasFluid());
            }
          }
          if (rb.readableBytes() > 0) {
            problems++;
            System.out.println("CHUNK LEFTOVER " + rb.readableBytes());
          }
        } catch (Exception e) {
          problems++;
          System.out.println("CHUNK SECTION FAILED: " + e);
        }
      }
      if (p instanceof ClientboundAddEntityPacket e) {
        if (e.getType() == zombieType) zombie = e.getId();
        if (e.getType() == sheepType) sheep = e.getId();
      }
      if (p instanceof ClientboundDamageEventPacket d && d.entityId() == zombie) damaged++;
      if (p instanceof ClientboundPlayerCombatKillPacket) {
        dead = true;
        send(sb, new ServerboundClientCommandPacket(ServerboundClientCommandPacket.Action.PERFORM_RESPAWN));
      }
      if (p instanceof ClientboundContainerSetContentPacket w && w.containerId() == 0) {
        for (int i = 36; i < 45 && i < w.items().size(); i++)
          if (w.items().get(i).is(net.minecraft.world.item.Items.CRAFTING_TABLE)) tableSlot = i - 36;
      }
      if (p instanceof ClientboundSystemChatPacket m) System.out.println("chat: " + m.content().getString());

      long t = System.currentTimeMillis() - start;
      if (watch) {
        // Only listens: sees what the server sends to everyone else.
      } else if (step == 0 && chunks > 50) {
        step++;
        for (String cmd : List.of("killall", "time set noon", "gamemode creative", "gamemode survival", "give diamond_sword 1",
            "summon zombie " + (px + 2) + " " + py + " " + pz, "summon sheep " + (px - 2) + " " + py + " " + pz))
          send(sb, new ServerboundChatCommandPacket(cmd));
      } else if (step == 1 && zombie >= 0 && t > 6000) {
        step++;
        send(sb, new ServerboundAttackPacket(zombie));
        send(sb, new ServerboundSwingPacket(InteractionHand.MAIN_HAND));
        send(sb, new ServerboundInteractPacket(zombie, InteractionHand.MAIN_HAND, net.minecraft.world.phys.Vec3.ZERO, false));
        send(sb, new ServerboundChatCommandPacket("tp " + px + " " + (py + 1) + " " + pz));
      } else if (step == 2 && t > 9000) {
        step++;
        for (String cmd : List.of("killall", "gamemode creative", "give crafting_table 1"))
          send(sb, new ServerboundChatCommandPacket(cmd));
      } else if (step == 3 && t > 11000) {
        step++;
        BlockPos below = BlockPos.containing(px, py - 2, pz);
        send(sb, new ServerboundPlayerActionPacket(ServerboundPlayerActionPacket.Action.START_DESTROY_BLOCK, below, Direction.UP, 1));
        if (tableSlot >= 0) send(sb, new ServerboundSetCarriedItemPacket(tableSlot));
        send(sb, new ServerboundUseItemOnPacket(InteractionHand.MAIN_HAND,
            new net.minecraft.world.phys.BlockHitResult(net.minecraft.world.phys.Vec3.atCenterOf(below.below()), Direction.UP, below.below(), false), 2));
      } else if (step == 4 && t > 13000) {
        step++;
        BlockPos below = BlockPos.containing(px, py - 2, pz);
        send(sb, new ServerboundUseItemOnPacket(InteractionHand.MAIN_HAND,
            new net.minecraft.world.phys.BlockHitResult(net.minecraft.world.phys.Vec3.atCenterOf(below), Direction.UP, below, false), 3));
      } else if (step == 5 && t > 15000) {
        step++;
        send(sb, new ServerboundContainerClosePacket(1));
        for (String cmd : List.of("gamemode survival", "summon creeper " + (px + 1.5) + " " + py + " " + pz))
          send(sb, new ServerboundChatCommandPacket(cmd));
      }
    }
    for (String want : List.of("play/level_event", "play/block_update", "play/open_screen", "play/explode", "play/sound", "play/respawn",
        "play/set_time", "play/damage_event", "play/take_item_entity"))
      if (!seen.containsKey(want)) System.out.println("NOT SEEN: " + want);
    System.out.println("chunks=" + chunks + " zombie=" + zombie + " sheep=" + sheep + " zombieDamaged=" + damaged);
    System.out.println("packets: " + seen);
    System.out.println(problems == 0 ? "OK: no wire problems" : "PROBLEMS: " + problems);
    s.close();
    System.exit(0);
  }

  static <T> void applyTags(Registry<T> r, TagNetworkSerialization.NetworkPayload p) {
    r.prepareTagReload(p.resolve(r)).apply();
  }

  static int nonEmpty(LevelChunkSection s) throws Exception {
    var f = LevelChunkSection.class.getDeclaredField("nonEmptyBlockCount");
    f.setAccessible(true);
    return f.getShort(s);
  }
}
